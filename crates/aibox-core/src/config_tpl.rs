//! 按 registry 的**写入模板**写配置（多格式：json / json5 / yaml / toml）。
//!
//! 为什么需要它：各应用的真实配置结构**完全不同**
//! （openclaw `models.providers.*.apiKey` / chatbox `settings.providers.*.apiKey` /
//! opencode `provider.*.options.apiKey` / aider YAML 顶层键 / continue 数组元素 /
//! moneyprinterturbo TOML `[app]` 段 …）。通用 JSON5 模板只对 openclaw 成立，
//! 对其余应用等于「写了个用户看不见、应用也不读的东西」。
//!
//! 安全不变式（与旧写路径一致，不放宽）：
//! - 解析不了原文件 → **拒写**（绝不覆盖用户文件）
//! - 未知 format / 未知 kind / 未知占位符 → fail-closed
//! - 数组元素找不到匹配 → `CONFIG_NO_TARGET`（**不臆造**条目）
//! - 写前备份 + 原子写 + 回读校验（由调用方 `config_write` 负责）

use serde_json::{Map, Value};

use crate::err::{CoreError, CoreResult};

/// 支持的写入格式。
pub const FORMATS: [&str; 4] = ["json", "json5", "yaml", "toml"];

/// 字段种类。
const KIND_SCALAR: &str = "scalar";
const KIND_LIST: &str = "list";
const KIND_ARRAY_ITEM: &str = "arrayItem";

/// 写入上下文：占位符取值来源。
pub struct Ctx<'a> {
    /// registry 的 provider id（`<provider>` 占位符）。
    pub provider: &'a str,
    pub api_key: Option<&'a str>,
    pub base_url: Option<&'a str>,
    pub model: Option<&'a str>,
}

/// 模板里的一个字段写入指令。
#[derive(Debug, Clone)]
pub struct Field {
    /// 目标路径：JSON/YAML 点分（`models.providers.openrouter.apiKey`）；TOML 点分（`app.llm_provider`）。
    pub path: String,
    /// 值：可含占位符（`<apiKey>` / `<model>` / `<baseUrl>` / `<provider>`），也可为字面量。
    pub value: Option<String>,
    /// 种类：`scalar`（默认）/ `list` / `arrayItem`。
    pub kind: String,
    /// `arrayItem`：命中条件（对象；所有键值都相等才算命中）。
    pub match_cond: Option<Value>,
    /// `arrayItem`：命中后要写入的字段（对象；值可含占位符）。
    pub set: Option<Value>,
    /// 占位符取值缺失时**跳过该字段**（可选字段，如 baseUrl / model）。
    pub optional: bool,
}

/// 写入模板（对应 registry `modelProviders.<pid>.configWrite`）。
#[derive(Debug, Clone)]
pub struct Template {
    pub format: String,
    /// 配置文件路径（**覆盖** registry 的 `configTarget`；例如 pi 的真实路径是 `~/.pi/agent/models.json`）。
    pub file: Option<String>,
    pub fields: Vec<Field>,
}

impl Template {
    pub fn supports_assisted_key(&self) -> bool {
        let supported = ["apiKey", "model", "baseUrl", "provider"];
        let check = |value: &str| {
            let mut rest = value;
            while let Some(start) = rest.find('<') {
                let tail = &rest[start + 1..];
                let Some(end) = tail.find('>') else {
                    return false;
                };
                if !supported.contains(&&tail[..end]) {
                    return false;
                }
                rest = &tail[end + 1..];
            }
            true
        };
        let mut key_field = false;
        for field in &self.fields {
            if field.kind == KIND_ARRAY_ITEM {
                let Some(set) = field.set.as_ref().and_then(Value::as_object) else {
                    return false;
                };
                let Some(matcher) = field.match_cond.as_ref().and_then(Value::as_object) else {
                    return false;
                };
                if matcher.is_empty()
                    || (self.format == "toml" && matcher.values().any(|value| !value.is_string()))
                    || set.is_empty()
                    || set.values().any(|value| !value.as_str().is_some_and(check))
                {
                    return false;
                }
                key_field |= set
                    .values()
                    .any(|value| value.as_str().is_some_and(|s| s.contains("<apiKey>")));
            } else if let Some(value) = field.value.as_deref() {
                if !check(value) {
                    return false;
                }
                key_field |= value.contains("<apiKey>");
            } else {
                return false;
            }
        }
        key_field
    }

    pub fn prepare_assisted<'a>(&self, ctx: &Ctx<'a>) -> CoreResult<PreparedTemplate<'a>> {
        if !self.supports_assisted_key() {
            return Err(CoreError::new(
                "CONFIG_FORMAT_UNSUPPORTED",
                "Unsupported or non-key configuration template",
            ));
        }
        for field in &self.fields {
            let values: Vec<&str> = if field.kind == KIND_ARRAY_ITEM {
                field
                    .set
                    .as_ref()
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        CoreError::new("CONFIG_TEMPLATE_INVALID", "arrayItem requires set object")
                    })?
                    .values()
                    .filter_map(Value::as_str)
                    .collect()
            } else {
                vec![field.value.as_deref().ok_or_else(|| {
                    CoreError::new("CONFIG_TEMPLATE_INVALID", "field value required")
                })?]
            };
            for value in values {
                match resolve_placeholders(value, ctx) {
                    Ok(_) => {}
                    Err(e) if field.optional && e.code == "CONFIG_NO_VALUES" => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(PreparedTemplate {
            template: self.clone(),
            ctx: Ctx {
                provider: ctx.provider,
                api_key: ctx.api_key,
                base_url: ctx.base_url,
                model: ctx.model,
            },
        })
    }

    /// 从 registry 文档片段解析模板（严格校验；任何不合规 → 报错，绝不放行）。
    pub fn from_value(v: &Value) -> CoreResult<Template> {
        let format = v
            .get("format")
            .and_then(|x| x.as_str())
            .ok_or_else(|| {
                CoreError::new("CONFIG_TEMPLATE_INVALID", "configWrite.format is required")
            })?
            .to_string();
        if !FORMATS.contains(&format.as_str()) {
            return Err(CoreError::new(
                "CONFIG_FORMAT_UNSUPPORTED",
                format!("Unsupported config format: {format}"),
            ));
        }
        let file = v
            .get("file")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        let raw_fields = v.get("fields").and_then(|x| x.as_array()).ok_or_else(|| {
            CoreError::new(
                "CONFIG_TEMPLATE_INVALID",
                "configWrite.fields must be an array",
            )
        })?;
        if raw_fields.is_empty() {
            return Err(CoreError::new(
                "CONFIG_TEMPLATE_INVALID",
                "configWrite.fields must not be empty",
            ));
        }
        let mut fields = Vec::new();
        for f in raw_fields {
            let path = f
                .get("path")
                .and_then(|x| x.as_str())
                .ok_or_else(|| {
                    CoreError::new(
                        "CONFIG_TEMPLATE_INVALID",
                        "configWrite field path is required",
                    )
                })?
                .to_string();
            if path.trim().is_empty() {
                return Err(CoreError::new(
                    "CONFIG_TEMPLATE_INVALID",
                    "configWrite field path must not be empty",
                ));
            }
            let kind = f
                .get("kind")
                .and_then(|x| x.as_str())
                .unwrap_or(KIND_SCALAR)
                .to_string();
            if !matches!(kind.as_str(), KIND_SCALAR | KIND_LIST | KIND_ARRAY_ITEM) {
                return Err(CoreError::new(
                    "CONFIG_TEMPLATE_INVALID",
                    format!("Unknown configWrite field kind: {kind}"),
                ));
            }
            let value = f
                .get("value")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());
            let match_cond = f.get("match").cloned();
            let set = f.get("set").cloned();
            if kind == KIND_ARRAY_ITEM && (match_cond.is_none() || set.is_none()) {
                return Err(CoreError::new(
                    "CONFIG_TEMPLATE_INVALID",
                    format!("arrayItem field requires match and set: {path}"),
                ));
            }
            if kind != KIND_ARRAY_ITEM && value.is_none() {
                return Err(CoreError::new(
                    "CONFIG_TEMPLATE_INVALID",
                    format!("configWrite field requires value: {path}"),
                ));
            }
            let optional = f.get("optional").and_then(|x| x.as_bool()).unwrap_or(false);
            fields.push(Field {
                path,
                value,
                kind,
                match_cond,
                set,
                optional,
            });
        }
        Ok(Template {
            format,
            file,
            fields,
        })
    }
}

pub struct PreparedTemplate<'a> {
    template: Template,
    ctx: Ctx<'a>,
}

impl PreparedTemplate<'_> {
    pub fn render(&self, existing: &str) -> CoreResult<(String, Vec<String>)> {
        if !self.ctx.api_key.is_some_and(|key| !key.trim().is_empty()) {
            return Err(CoreError::new("CONFIG_NO_VALUES", "An API key is required"));
        }
        let (text, applied) = render_with_provenance(&self.template, existing, &self.ctx)?;
        let verification_error = || {
            CoreError::new(
                "CONFIG_WRITE_FAILED",
                "Rendered configuration verification failed",
            )
        };
        let verified = if applied.key_writes.is_empty() {
            false
        } else if self.template.format == "toml" {
            let doc: toml_edit::Document = text.parse().map_err(|_| verification_error())?;
            applied.matches(|path| {
                path.iter()
                    .try_fold(doc.as_item(), |item, part| match part {
                        PathPart::Key(key) => item.get(key),
                        PathPart::Index(index) => item.get(*index),
                    })?
                    .as_str()
            })
        } else {
            let root = match self.template.format.as_str() {
                "json" => parse_json_strict(&text),
                "json5" => parse_json5(&text),
                "yaml" => parse_yaml(&text),
                _ => unreachable!("renderer returned an unsupported format"),
            }
            .map_err(|_| verification_error())?;
            applied.matches(|path| {
                path.iter()
                    .try_fold(&root, |value, part| match part {
                        PathPart::Key(key) => value.get(key.as_str()),
                        PathPart::Index(index) => value.get(*index),
                    })?
                    .as_str()
            })
        };
        if !verified {
            return Err(CoreError::new(
                "CONFIG_NO_VALUES",
                "No API key field was applied to the rendered configuration",
            ));
        }
        Ok((text, applied.changed))
    }
}

#[derive(PartialEq)]
enum PathPart {
    Key(String),
    Index(usize),
}

#[derive(Default)]
struct AppliedFields {
    changed: Vec<String>,
    key_writes: Vec<(Vec<PathPart>, String)>,
}

impl AppliedFields {
    fn record(
        &mut self,
        field: &Field,
        index: Option<usize>,
        member: Option<&str>,
        key: Option<&str>,
    ) {
        let (parent, leaf) = field.path.rsplit_once('.').unwrap_or(("", &field.path));
        let mut path: Vec<_> = parent
            .split('.')
            .filter(|part| !part.is_empty())
            .map(|part| PathPart::Key(part.into()))
            .collect();
        if !leaf.is_empty() || index.is_some() {
            path.push(PathPart::Key(leaf.into()));
        }
        if let Some(index) = index {
            path.push(PathPart::Index(index));
        }
        if let Some(member) = member {
            path.push(PathPart::Key(member.into()));
        }
        // Replacing a leaf or its ancestor invalidates the previous write.
        self.key_writes
            .retain(|(previous, _)| !previous.starts_with(&path) && !path.starts_with(previous));
        if let Some(key) = key {
            self.key_writes.push((path, key.into()));
        }
        self.changed.push(match member {
            Some(member) => format!("{}.{member}", field.path),
            None => field.path.clone(),
        });
    }

    fn matches<'a>(&self, read: impl Fn(&[PathPart]) -> Option<&'a str>) -> bool {
        self.key_writes
            .iter()
            .any(|(path, value)| read(path) == Some(value.as_str()))
    }
}

/// 解析占位符：`&lt;apiKey&gt;` / `&lt;model&gt;` / `&lt;baseUrl&gt;` / `&lt;provider&gt;`。
/// 未知占位符 → 报错（fail-closed）；需要的值缺失 → `CONFIG_NO_VALUES`。
fn resolve_placeholders(raw: &str, ctx: &Ctx) -> CoreResult<String> {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(pos) = rest.find('<') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let end = tail.find('>').ok_or_else(|| {
            CoreError::new(
                "CONFIG_TEMPLATE_INVALID",
                "unterminated placeholder in configWrite",
            )
        })?;
        let name = &tail[1..end];
        let value = match name {
            "apiKey" => ctx.api_key.filter(|v| !v.trim().is_empty()),
            "model" => ctx.model.filter(|v| !v.trim().is_empty()),
            "baseUrl" => ctx.base_url.filter(|v| !v.trim().is_empty()),
            "provider" => Some(ctx.provider),
            _ => {
                return Err(CoreError::new(
                    "CONFIG_TEMPLATE_INVALID",
                    format!("Unknown placeholder: <{name}>"),
                ))
            }
        };
        let value = value.ok_or_else(|| {
            CoreError::new(
                "CONFIG_NO_VALUES",
                format!("Missing value for placeholder: <{name}>"),
            )
        })?;
        out.push_str(value);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// 按模板渲染新配置文本。返回 `(新文本, 变更的键路径)`。
pub fn render(tpl: &Template, existing: &str, ctx: &Ctx) -> CoreResult<(String, Vec<String>)> {
    let (text, applied) = render_with_provenance(tpl, existing, ctx)?;
    Ok((text, applied.changed))
}

fn render_with_provenance(
    tpl: &Template,
    existing: &str,
    ctx: &Ctx,
) -> CoreResult<(String, AppliedFields)> {
    match tpl.format.as_str() {
        "json" => {
            let mut root = parse_json_strict(existing)?;
            let changed = apply_fields(&mut root, &tpl.fields, ctx)?;
            let text = serde_json::to_string_pretty(&root).map_err(|e| {
                CoreError::new("CONFIG_WRITE_FAILED", format!("cannot emit JSON: {e}"))
            })?;
            Ok((text, changed))
        }
        "json5" => {
            let mut root = parse_json5(existing)?;
            let changed = apply_fields(&mut root, &tpl.fields, ctx)?;
            let text = json5::to_string(&root).map_err(|e| {
                CoreError::new("CONFIG_WRITE_FAILED", format!("cannot emit JSON5: {e}"))
            })?;
            Ok((text, changed))
        }
        "yaml" => {
            let mut root = parse_yaml(existing)?;
            let changed = apply_fields(&mut root, &tpl.fields, ctx)?;
            Ok((render_yaml(&root)?, changed))
        }
        "toml" => {
            let mut doc: toml_edit::Document = existing.parse().map_err(|e| {
                CoreError::new(
                    "CONFIG_PARSE_FAILED",
                    format!("cannot parse TOML config: {e}"),
                )
            })?;
            let changed = apply_fields_toml(&mut doc, &tpl.fields, ctx)?;
            Ok((doc.to_string(), changed))
        }
        other => Err(CoreError::new(
            "CONFIG_FORMAT_UNSUPPORTED",
            format!("Unsupported config format: {other}"),
        )),
    }
}

// ---------------------------------------------------------------- JSON / JSON5 / YAML 共用（serde_json::Value 树）

fn parse_json_strict(text: &str) -> CoreResult<Value> {
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(text).map_err(|e| {
        CoreError::new(
            "CONFIG_PARSE_FAILED",
            format!("cannot parse JSON config: {e}"),
        )
    })
}

fn parse_json5(text: &str) -> CoreResult<Value> {
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    json5::from_str(text).map_err(|e| {
        CoreError::new(
            "CONFIG_PARSE_FAILED",
            format!("cannot parse JSON5 config: {e}"),
        )
    })
}

/// 在 `root` 上应用字段（就地修改；记录实际写入）。
fn apply_fields(root: &mut Value, fields: &[Field], ctx: &Ctx) -> CoreResult<AppliedFields> {
    let mut applied = AppliedFields::default();
    for f in fields {
        match f.kind.as_str() {
            KIND_SCALAR => {
                let raw = f.value.as_deref().unwrap_or_default();
                let val = match resolve_placeholders(raw, ctx) {
                    Ok(v) => v,
                    Err(e) if f.optional && e.code == "CONFIG_NO_VALUES" => continue,
                    Err(e) => return Err(e),
                };
                set_path(root, &f.path, Value::String(val.clone()))?;
                applied.record(f, None, None, raw.contains("<apiKey>").then_some(&val));
            }
            KIND_LIST => {
                let raw = f.value.as_deref().unwrap_or_default();
                let val = match resolve_placeholders(raw, ctx) {
                    Ok(v) => v,
                    Err(e) if f.optional && e.code == "CONFIG_NO_VALUES" => continue,
                    Err(e) => return Err(e),
                };
                let index = apply_list(root, &f.path, &val)?;
                applied.record(
                    f,
                    Some(index),
                    None,
                    raw.contains("<apiKey>").then_some(&val),
                );
            }
            KIND_ARRAY_ITEM => {
                apply_array_item(root, f, ctx, &mut applied)?;
            }
            other => {
                return Err(CoreError::new(
                    "CONFIG_TEMPLATE_INVALID",
                    format!("Unknown configWrite field kind: {other}"),
                ))
            }
        }
    }
    Ok(applied)
}

/// 点分路径写值（中间层缺失则创建对象）。
fn set_path(root: &mut Value, path: &str, val: Value) -> CoreResult<()> {
    let parts: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return Err(CoreError::new(
            "CONFIG_TEMPLATE_INVALID",
            "configWrite field path must not be empty",
        ));
    }
    let mut cur = root;
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        if !cur.is_object() {
            *cur = Value::Object(Map::new());
        }
        let map = cur.as_object_mut().expect("object");
        if last {
            map.insert((*part).to_string(), val);
            return Ok(());
        }
        let entry = map
            .entry((*part).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        cur = entry;
    }
    Ok(())
}

/// 列表写入：数组里的 `k=v` 条目 —— 同 `k` 存在则替换，否则追加。
/// 例：aider 的 `api-key: [openrouter=sk-xxx]`。
fn apply_list(root: &mut Value, path: &str, entry: &str) -> CoreResult<usize> {
    let key = entry.split('=').next().unwrap_or(entry).trim().to_string();
    let parent_path = match path.rfind('.') {
        Some(i) => &path[..i],
        None => "",
    };
    let leaf = match path.rfind('.') {
        Some(i) => &path[i + 1..],
        None => path,
    };
    let container = if parent_path.is_empty() {
        root
    } else {
        get_or_create_object(root, parent_path)?
    };
    if !container.is_object() {
        *container = Value::Object(Map::new());
    }
    let map = container.as_object_mut().expect("object");
    let list = map
        .entry(leaf.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !list.is_array() {
        *list = Value::Array(Vec::new());
    }
    let arr = list.as_array_mut().expect("array");
    for (index, item) in arr.iter_mut().enumerate() {
        let k = item
            .as_str()
            .unwrap_or_default()
            .split('=')
            .next()
            .unwrap_or("")
            .trim();
        if k == key {
            *item = Value::String(entry.to_string());
            return Ok(index);
        }
    }
    arr.push(Value::String(entry.to_string()));
    Ok(arr.len() - 1)
}

/// 取（或创建）中间对象。
fn get_or_create_object<'a>(root: &'a mut Value, path: &str) -> CoreResult<&'a mut Value> {
    let mut cur = root;
    for part in path.split('.').filter(|s| !s.is_empty()) {
        if !cur.is_object() {
            *cur = Value::Object(Map::new());
        }
        let map = cur.as_object_mut().expect("object");
        let entry = map
            .entry(part.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        cur = entry;
    }
    Ok(cur)
}

/// 数组元素写入：按 `match` 找到元素 → 写 `set` 字段。
/// 找不到 → `CONFIG_NO_TARGET`（**不臆造**条目）。
fn apply_array_item(
    root: &mut Value,
    f: &Field,
    ctx: &Ctx,
    applied: &mut AppliedFields,
) -> CoreResult<()> {
    let match_obj = f
        .match_cond
        .as_ref()
        .and_then(|v| v.as_object())
        .ok_or_else(|| {
            CoreError::new(
                "CONFIG_TEMPLATE_INVALID",
                "arrayItem requires a match object",
            )
        })?;
    let Some(set) = resolve_array_set(f, ctx)? else {
        return Ok(());
    };
    let arr_path = match f.path.rfind('.') {
        Some(i) => &f.path[..i],
        None => "",
    };
    let leaf = match f.path.rfind('.') {
        Some(i) => &f.path[i + 1..],
        None => f.path.as_str(),
    };
    let list = if arr_path.is_empty() {
        root.get(leaf).cloned().unwrap_or(Value::Array(Vec::new()))
    } else {
        get_or_create_object(root, arr_path)?
            .get(leaf)
            .cloned()
            .unwrap_or(Value::Array(Vec::new()))
    };
    let mut arr = match list {
        Value::Array(a) => a,
        _ => Vec::new(),
    };
    let mut hit: Option<usize> = None;
    for (i, item) in arr.iter().enumerate() {
        let obj = match item.as_object() {
            Some(o) => o,
            None => continue,
        };
        let all_match = match_obj.iter().all(|(k, want)| {
            let got = obj.get(k);
            match (got, want) {
                (Some(g), w) => values_equal(g, w),
                (None, Value::Null) => true,
                _ => false,
            }
        });
        if all_match {
            hit = Some(i);
            break;
        }
    }
    let idx = hit.ok_or_else(|| {
        CoreError::new(
            "CONFIG_NO_TARGET",
            format!("no array element matched for path: {}", f.path),
        )
    })?;
    let obj = arr[idx].as_object_mut().expect("matched object");
    for member in set {
        member.record(f, idx, applied);
        obj.insert(member.name, member.value);
    }
    let container = if arr_path.is_empty() {
        root
    } else {
        get_or_create_object(root, arr_path)?
    };
    container
        .as_object_mut()
        .expect("matched array belongs to an object")
        .insert(leaf.to_string(), Value::Array(arr));
    Ok(())
}

struct ResolvedSetField {
    name: String,
    value: Value,
    has_key: bool,
}

impl ResolvedSetField {
    fn record(&self, field: &Field, index: usize, applied: &mut AppliedFields) {
        let key = self.value.as_str().filter(|_| self.has_key);
        applied.record(field, Some(index), Some(&self.name), key);
    }
}

fn resolve_array_set(f: &Field, ctx: &Ctx) -> CoreResult<Option<Vec<ResolvedSetField>>> {
    let set = f.set.as_ref().and_then(Value::as_object).ok_or_else(|| {
        CoreError::new("CONFIG_TEMPLATE_INVALID", "arrayItem requires a set object")
    })?;
    let mut resolved = Vec::with_capacity(set.len());
    let mut missing = false;
    for (name, value) in set {
        let (value, has_key) = match value {
            Value::String(raw) => match resolve_placeholders(raw, ctx) {
                Ok(value) => (Value::String(value), raw.contains("<apiKey>")),
                Err(e) if f.optional && e.code == "CONFIG_NO_VALUES" => {
                    missing = true;
                    continue;
                }
                Err(e) => return Err(e),
            },
            value => (value.clone(), false),
        };
        resolved.push(ResolvedSetField {
            name: name.clone(),
            value,
            has_key,
        });
    }
    // Optional applies to the entire field. Resolve before touching the document.
    Ok((!missing).then_some(resolved))
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), y) => y.as_str().map(|s| s == x).unwrap_or(false),
        (x, y) => x == y,
    }
}

// ---------------------------------------------------------------- TOML（toml_edit：保留注释与未知键）

fn apply_fields_toml(
    doc: &mut toml_edit::Document,
    fields: &[Field],
    ctx: &Ctx,
) -> CoreResult<AppliedFields> {
    let mut applied = AppliedFields::default();
    for f in fields {
        match f.kind.as_str() {
            KIND_SCALAR => {
                let raw = f.value.as_deref().unwrap_or_default();
                let val = match resolve_placeholders(raw, ctx) {
                    Ok(v) => v,
                    Err(e) if f.optional && e.code == "CONFIG_NO_VALUES" => continue,
                    Err(e) => return Err(e),
                };
                toml_set(doc, &f.path, &val)?;
                applied.record(f, None, None, raw.contains("<apiKey>").then_some(&val));
            }
            KIND_LIST => {
                let raw = f.value.as_deref().unwrap_or_default();
                let val = match resolve_placeholders(raw, ctx) {
                    Ok(v) => v,
                    Err(e) if f.optional && e.code == "CONFIG_NO_VALUES" => continue,
                    Err(e) => return Err(e),
                };
                let index = toml_list(doc, &f.path, &val)?;
                applied.record(
                    f,
                    Some(index),
                    None,
                    raw.contains("<apiKey>").then_some(&val),
                );
            }
            KIND_ARRAY_ITEM => {
                toml_array_item(doc, f, ctx, &mut applied)?;
            }
            other => {
                return Err(CoreError::new(
                    "CONFIG_TEMPLATE_INVALID",
                    format!("Unknown configWrite field kind: {other}"),
                ))
            }
        }
    }
    Ok(applied)
}

fn toml_set(doc: &mut toml_edit::Document, path: &str, val: &str) -> CoreResult<()> {
    let parts: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return Err(CoreError::new(
            "CONFIG_TEMPLATE_INVALID",
            "configWrite field path must not be empty",
        ));
    }
    let mut table = doc.as_table_mut();
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        if last {
            table.insert(part, toml_edit::value(val));
            return Ok(());
        }
        let entry = table
            .entry(part)
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
        if !entry.is_table() {
            *entry = toml_edit::Item::Table(toml_edit::Table::new());
        }
        table = entry.as_table_mut().expect("table");
    }
    Ok(())
}

fn toml_list(doc: &mut toml_edit::Document, path: &str, entry: &str) -> CoreResult<usize> {
    let key = entry.split('=').next().unwrap_or(entry).trim().to_string();
    let (parent_path, leaf) = match path.rfind('.') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    };
    let mut table = doc.as_table_mut();
    if !parent_path.is_empty() {
        for part in parent_path.split('.').filter(|s| !s.is_empty()) {
            let e = table
                .entry(part)
                .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
            if !e.is_table() {
                *e = toml_edit::Item::Table(toml_edit::Table::new());
            }
            table = e.as_table_mut().expect("table");
        }
    }
    let mut arr: Vec<toml_edit::Value> = table
        .get(leaf)
        .and_then(|i| i.as_array())
        .map(|a| a.iter().cloned().collect())
        .unwrap_or_default();
    let index = arr.iter().position(|item| {
        item.as_str()
            .unwrap_or_default()
            .split('=')
            .next()
            .unwrap_or("")
            .trim()
            == key
    });
    let index = if let Some(index) = index {
        arr[index] = toml_edit::Value::from(entry);
        index
    } else {
        arr.push(toml_edit::Value::from(entry));
        arr.len() - 1
    };
    let mut out_arr = toml_edit::Array::new();
    for v in arr {
        out_arr.push(v);
    }
    table.insert(
        leaf,
        toml_edit::Item::Value(toml_edit::Value::Array(out_arr)),
    );
    Ok(index)
}

/// 从 toml_edit::Value（内联表或表）里取字符串字段。
fn toml_value_get_str(v: &toml_edit::Value, key: &str) -> Option<String> {
    match v {
        toml_edit::Value::InlineTable(t) => {
            t.get(key).and_then(|i| i.as_str()).map(|s| s.to_string())
        }
        _ => None,
    }
}

fn toml_array_item(
    doc: &mut toml_edit::Document,
    f: &Field,
    ctx: &Ctx,
    applied: &mut AppliedFields,
) -> CoreResult<()> {
    let match_obj = f
        .match_cond
        .as_ref()
        .and_then(|v| v.as_object())
        .ok_or_else(|| {
            CoreError::new(
                "CONFIG_TEMPLATE_INVALID",
                "arrayItem requires a match object",
            )
        })?;
    let Some(set) = resolve_array_set(f, ctx)? else {
        return Ok(());
    };
    let (parent_path, leaf) = match f.path.rfind('.') {
        Some(i) => (&f.path[..i], &f.path[i + 1..]),
        None => ("", f.path.as_str()),
    };
    let mut table = doc.as_table_mut();
    if !parent_path.is_empty() {
        for part in parent_path.split('.').filter(|s| !s.is_empty()) {
            let e = table
                .entry(part)
                .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
            if !e.is_table() {
                *e = toml_edit::Item::Table(toml_edit::Table::new());
            }
            table = e.as_table_mut().expect("table");
        }
    }
    let mut arr: Vec<toml_edit::Value> = table
        .get(leaf)
        .and_then(|i| i.as_array())
        .map(|a| a.iter().cloned().collect())
        .unwrap_or_default();
    let mut hit: Option<usize> = None;
    for (i, item) in arr.iter().enumerate() {
        if item.as_inline_table().is_none() {
            continue;
        }
        let mut all_match = true;
        for (k, want) in match_obj.iter() {
            let got = toml_value_get_str(item, k);
            let want_s = want.as_str().ok_or_else(|| {
                CoreError::new("CONFIG_TEMPLATE_INVALID", "TOML array matches require strings")
            })?;
            if got.as_deref() != Some(want_s) {
                all_match = false;
                break;
            }
        }
        if all_match {
            hit = Some(i);
            break;
        }
    }
    let idx = hit.ok_or_else(|| {
        CoreError::new(
            "CONFIG_NO_TARGET",
            format!("no array element matched for path: {}", f.path),
        )
    })?;
    let inline = arr[idx]
        .as_inline_table_mut()
        .expect("matched inline table");
    for member in set {
        let resolved = match &member.value {
            Value::String(value) => toml_edit::Value::from(value.as_str()),
            other => toml_edit::Value::from(other.to_string()),
        };
        inline.insert(&member.name, resolved);
        member.record(f, idx, applied);
    }
    let mut out_arr = toml_edit::Array::new();
    for v in arr {
        out_arr.push(v);
    }
    table.insert(
        leaf,
        toml_edit::Item::Value(toml_edit::Value::Array(out_arr)),
    );
    Ok(())
}

// ---------------------------------------------------------------- YAML 解析/渲染

fn parse_yaml(text: &str) -> CoreResult<Value> {
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let docs = yaml_rust2::YamlLoader::load_from_str(text).map_err(|e| {
        CoreError::new(
            "CONFIG_PARSE_FAILED",
            format!("cannot parse YAML config: {e}"),
        )
    })?;
    let first = docs.into_iter().next();
    Ok(match first {
        Some(yaml_rust2::Yaml::Hash(h)) => {
            let mut m = Map::new();
            for (k, v) in h {
                m.insert(yaml_key(&k), yaml_to_json(&v));
            }
            Value::Object(m)
        }
        Some(other) => yaml_to_json(&other),
        None => Value::Object(Map::new()),
    })
}

fn yaml_key(k: &yaml_rust2::Yaml) -> String {
    match k {
        yaml_rust2::Yaml::String(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

fn yaml_to_json(y: &yaml_rust2::Yaml) -> Value {
    use yaml_rust2::Yaml;
    match y {
        Yaml::String(s) => Value::String(s.clone()),
        Yaml::Integer(i) => Value::from(*i),
        Yaml::Real(s) => s
            .parse::<f64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(s.clone())),
        Yaml::Boolean(b) => Value::Bool(*b),
        Yaml::Null => Value::Null,
        Yaml::Alias(_) => Value::Null,
        Yaml::BadValue => Value::Null,
        Yaml::Array(a) => Value::Array(a.iter().map(yaml_to_json).collect()),
        Yaml::Hash(h) => {
            let mut m = Map::new();
            for (k, v) in h {
                m.insert(yaml_key(k), yaml_to_json(v));
            }
            Value::Object(m)
        }
    }
}

fn json_to_yaml(v: &Value) -> yaml_rust2::Yaml {
    use yaml_rust2::Yaml;
    match v {
        Value::Null => Yaml::Null,
        Value::Bool(b) => Yaml::Boolean(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Yaml::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Yaml::Real(f.to_string())
            } else {
                Yaml::String(n.to_string())
            }
        }
        Value::String(s) => Yaml::String(s.clone()),
        Value::Array(a) => Yaml::Array(a.iter().map(json_to_yaml).collect()),
        Value::Object(m) => {
            let mut h = yaml_rust2::yaml::Hash::new();
            for (k, val) in m {
                h.insert(yaml_rust2::Yaml::String(k.clone()), json_to_yaml(val));
            }
            Yaml::Hash(h)
        }
    }
}

fn render_yaml(v: &Value) -> CoreResult<String> {
    let mut out = String::new();
    let mut emitter = yaml_rust2::YamlEmitter::new(&mut out);
    emitter
        .dump(&json_to_yaml(v))
        .map_err(|e| CoreError::new("CONFIG_WRITE_FAILED", format!("cannot emit YAML: {e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> Ctx<'static> {
        Ctx {
            provider: "openrouter",
            api_key: Some("sk-or-v1-test-key"),
            base_url: Some("https://openrouter.ai/api/v1"),
            model: Some("anthropic/claude-sonnet-4"),
        }
    }

    fn tpl(format: &str, fields: Vec<Field>) -> Template {
        Template {
            format: format.into(),
            file: None,
            fields,
        }
    }

    fn f(path: &str, value: &str) -> Field {
        Field {
            path: path.into(),
            value: Some(value.into()),
            kind: KIND_SCALAR.into(),
            match_cond: None,
            set: None,
            optional: false,
        }
    }

    #[test]
    fn json_nested_path_creates_intermediates_and_keeps_unknown_keys() {
        let t = tpl(
            "json",
            vec![f("models.providers.openrouter.apiKey", "<apiKey>")],
        );
        let existing = r#"{"theme":"dark","models":{"other":{"apiKey":"x"}}}"#;
        let (text, changed) = render(&t, existing, &ctx()).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            v.pointer("/models/providers/openrouter/apiKey").unwrap(),
            "sk-or-v1-test-key"
        );
        // 未知键保留
        assert_eq!(v.pointer("/theme").unwrap(), "dark");
        assert_eq!(v.pointer("/models/other/apiKey").unwrap(), "x");
        assert_eq!(
            changed,
            vec!["models.providers.openrouter.apiKey".to_string()]
        );
    }

    #[test]
    fn json_flat_key_layout() {
        let t = tpl("json", vec![f("open_ai_api_key", "<apiKey>")]);
        let (text, _) = render(&t, "{}", &ctx()).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["open_ai_api_key"], "sk-or-v1-test-key");
    }

    #[test]
    fn json5_keeps_comments_style_input() {
        let t = tpl(
            "json5",
            vec![f("models.providers.openrouter.apiKey", "<apiKey>")],
        );
        let existing = "{\n  // 注释\n  a: 1,\n}";
        let (text, _) = render(&t, existing, &ctx()).unwrap();
        assert!(text.contains("sk-or-v1-test-key"));
    }

    #[test]
    fn yaml_top_level_key_and_unknown_key_preserved() {
        let t = tpl("yaml", vec![f("openai-api-key", "<apiKey>")]);
        let existing = "model: gpt-4\nread: file\n";
        let (text, _) = render(&t, existing, &ctx()).unwrap();
        let back = parse_yaml(&text).unwrap();
        assert_eq!(back["openai-api-key"], "sk-or-v1-test-key");
        assert_eq!(back["model"], "gpt-4");
        assert_eq!(back["read"], "file");
    }

    #[test]
    fn yaml_list_entry_replaces_same_key() {
        let t = tpl(
            "yaml",
            vec![Field {
                path: "api-key".into(),
                value: Some("<provider>=<apiKey>".into()),
                kind: KIND_LIST.into(),
                match_cond: None,
                set: None,
                optional: false,
            }],
        );
        let mut context = ctx();
        context.api_key = Some("test-key==");
        let prepared = t.prepare_assisted(&context).unwrap();
        // 同 key → 替换（不重复写）
        let (text, _) = prepared
            .render("api-key:\n  - openrouter=sk-old\n")
            .unwrap();
        let back = parse_yaml(&text).unwrap();
        let arr = back["api-key"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "同 key 应替换而非追加：{text}");
        assert_eq!(arr[0], "openrouter=test-key==");
        // 不同 key → 共存追加（aider 支持多个 provider）
        let (text, _) = prepared.render("api-key:\n  - openai=sk-old\n").unwrap();
        let back = parse_yaml(&text).unwrap();
        let arr = back["api-key"].as_array().unwrap();
        assert_eq!(arr.len(), 2, "不同 provider 应共存：{text}");
        assert_eq!(arr[1], "openrouter=test-key==");
    }

    #[test]
    fn yaml_array_item_requires_match_and_refuses_to_fabricate() {
        let t = tpl(
            "yaml",
            vec![Field {
                path: "models".into(),
                value: None,
                kind: KIND_ARRAY_ITEM.into(),
                match_cond: Some(json!({"provider": "openrouter"})),
                set: Some(json!({"apiKey": "<apiKey>"})),
                optional: false,
            }],
        );
        let context = ctx();
        let prepared = t.prepare_assisted(&context).unwrap();
        let (text, changed) = prepared.render(
            "models:\n  - name: m1\n    provider: openai\n  - name: m2\n    provider: openrouter\n",
        ).unwrap();
        let back = parse_yaml(&text).unwrap();
        assert_eq!(
            back["models"][1]["apiKey"], "sk-or-v1-test-key",
            "命中的元素应被写入：{text}"
        );
        assert!(back["models"][0].get("apiKey").is_none());
        assert_eq!(changed, vec!["models.apiKey"]);
        // 无匹配项 → 拒写（不臆造条目）
        let err = prepared
            .render("models:\n  - name: m1\n    provider: openai\n")
            .unwrap_err();
        assert_eq!(err.code, "CONFIG_NO_TARGET");
    }

    #[test]
    fn toml_table_and_unknown_keys_preserved() {
        let t = tpl(
            "toml",
            vec![
                f("app.llm_provider", "openrouter"),
                f("app.openrouter_api_key", "<apiKey>"),
            ],
        );
        let existing =
            "# 顶部注释\n[app]\nllm_provider = \"openai\"\nother = 1\n\n[server]\nport = 8080\n";
        let context = ctx();
        let (text, changed) = t
            .prepare_assisted(&context)
            .unwrap()
            .render(existing)
            .unwrap();
        assert_eq!(changed, vec!["app.llm_provider", "app.openrouter_api_key"]);
        assert!(text.contains("llm_provider = \"openrouter\""), "{text}");
        assert!(
            text.contains("openrouter_api_key = \"sk-or-v1-test-key\""),
            "{text}"
        );
        assert!(text.contains("other = 1"), "未知键要保留：{text}");
        assert!(text.contains("port = 8080"), "其他段要保留：{text}");
        assert!(text.contains("# 顶部注释"), "注释要保留：{text}");
    }

    #[test]
    fn unsupported_format_and_unknown_placeholder_fail_closed() {
        let t = tpl("sqlite", vec![f("a", "<apiKey>")]);
        let err = render(&t, "{}", &ctx()).unwrap_err();
        assert_eq!(err.code, "CONFIG_FORMAT_UNSUPPORTED");

        let t = tpl("json", vec![f("a", "<secret>")]);
        let err = render(&t, "{}", &ctx()).unwrap_err();
        assert_eq!(err.code, "CONFIG_TEMPLATE_INVALID");
    }

    #[test]
    fn missing_value_is_refused_unless_optional() {
        let mut c = ctx();
        c.api_key = None;
        let t = tpl("json", vec![f("a", "<apiKey>")]);
        let err = render(&t, "{}", &c).unwrap_err();
        assert_eq!(err.code, "CONFIG_NO_VALUES");

        let mut opt = f("b", "<apiKey>");
        opt.optional = true;
        let t = tpl("json", vec![opt]);
        let (text, changed) = render(&t, "{}", &c).unwrap();
        assert!(changed.is_empty());
        assert!(!text.contains("\"b\""));
    }

    #[test]
    fn assisted_key_does_not_require_optional_model_and_url() {
        let mut context = ctx();
        context.model = None;
        context.base_url = None;
        let mut model = f("model", "<model>");
        model.optional = true;
        let mut url = f("url", "<baseUrl>");
        url.optional = true;
        let template = tpl("json", vec![f("key", "<apiKey>"), model, url]);
        let prepared = template.prepare_assisted(&context).unwrap();
        let (_, changed) = prepared.render("{}").unwrap();
        assert_eq!(changed, vec!["key"]);
    }

    #[test]
    fn assisted_toml_array_item_verifies_exact_member_and_index() {
        let t = Template::from_value(&json!({"format":"toml", "fields":[{
            "kind":"arrayItem", "path":"app.models", "match":{"provider":"openrouter"},
            "set":{"api.key":"<apiKey>"}
        }]}))
        .unwrap();
        let context = ctx();
        let (text, changed) = t
            .prepare_assisted(&context)
            .unwrap()
            .render("[app]\nmodels = [{provider = 'openai'}, {provider = 'openrouter'}]\n")
            .unwrap();
        let doc: toml_edit::Document = text.parse().unwrap();
        assert_eq!(doc["app"]["models"][1]["api.key"].as_str(), context.api_key);
        assert_eq!(changed, vec!["app.models.api.key"]);
    }

    #[test]
    fn optional_array_item_skips_whole_set_when_value_is_missing() {
        for (format, existing) in [
            (
                "yaml",
                "models:\n  - provider: openrouter\n    apiKey: old\n",
            ),
            (
                "toml",
                "models = [{provider = 'openrouter', apiKey = 'old'}]\n",
            ),
        ] {
            let mut t = Template::from_value(&json!({"format":format, "fields":[{
                "kind":"arrayItem", "path":"models", "match":{"provider":"openrouter"},
                "set":{"apiKey":"<apiKey>", "model":"<model>"}, "optional":true
            }, {"path":"enabled", "value":"yes"}]}))
            .unwrap();
            let mut context = ctx();
            context.model = None;
            let err = t
                .prepare_assisted(&context)
                .unwrap()
                .render(existing)
                .unwrap_err();
            assert_eq!(err.code, "CONFIG_NO_VALUES", "{format}");

            t.fields.push(f("key", "<apiKey>"));
            let (text, changed) = t
                .prepare_assisted(&context)
                .unwrap()
                .render(existing)
                .unwrap();
            assert_eq!(changed, vec!["enabled", "key"], "{format}");
            assert!(text.contains("old"), "{format}");
        }
    }

    #[test]
    fn assisted_key_must_survive_later_field_writes() {
        let t = tpl(
            "json5",
            vec![f("settings.key", "Bearer <apiKey>"), f("settings", "reset")],
        );
        let context = ctx();
        let err = t
            .prepare_assisted(&context)
            .unwrap()
            .render("{}")
            .unwrap_err();
        assert_eq!(err.code, "CONFIG_NO_VALUES");
    }

    #[test]
    fn unparsable_original_is_refused_and_file_untouched() {
        let t = tpl("json", vec![f("a", "<apiKey>")]);
        let err = render(&t, "{ this is not json", &ctx()).unwrap_err();
        assert_eq!(err.code, "CONFIG_PARSE_FAILED");
    }

    #[test]
    fn template_parsing_rejects_bad_shapes() {
        let bad = json!({"format":"json"});
        assert_eq!(
            Template::from_value(&bad).unwrap_err().code,
            "CONFIG_TEMPLATE_INVALID"
        );
        let bad = json!({"format":"json","fields":[]});
        assert_eq!(
            Template::from_value(&bad).unwrap_err().code,
            "CONFIG_TEMPLATE_INVALID"
        );
        let bad = json!({"format":"json","fields":[{"path":"a","kind":"weird","value":"x"}]});
        assert_eq!(
            Template::from_value(&bad).unwrap_err().code,
            "CONFIG_TEMPLATE_INVALID"
        );
        let bad = json!({"format":"json","fields":[{"path":"a","kind":"arrayItem"}]});
        assert_eq!(
            Template::from_value(&bad).unwrap_err().code,
            "CONFIG_TEMPLATE_INVALID"
        );
        let ok = json!({"format":"json","fields":[{"path":"a","value":"<apiKey>"}]});
        let t = Template::from_value(&ok).unwrap();
        assert_eq!(t.format, "json");
        assert_eq!(t.fields.len(), 1);
    }
}
