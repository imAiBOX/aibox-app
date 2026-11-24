// Versioned, read-only catalog classification. Vendored verbatim in aibox-site.
export const CONFIG_CAPABILITY_VERSION = 1;
export const CONFIG_PLATFORMS = ['windows', 'macos'] as const;
export const CONFIG_PROVIDERS = ['openrouter', 'anthropic', 'openai'] as const;
const FORMATS = ['json', 'json5', 'yaml', 'toml'];

export type ConfigPlatform = typeof CONFIG_PLATFORMS[number];
export type ConfigCapability = 'assisted' | 'manual' | 'unavailable';

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function fieldValues(field: unknown, format: string): string[] | undefined {
  if (!isRecord(field) || typeof field.path !== 'string' || !field.path.trim()) return undefined;
  const kind = field.kind === undefined ? 'scalar' : field.kind;
  if (kind === 'arrayItem' && isRecord(field.match) && isRecord(field.set) && Object.keys(field.set).length > 0) {
    const matchValues = Object.values(field.match);
    if (!matchValues.length || (format === 'toml' && matchValues.some((value) => typeof value !== 'string'))) return undefined;
    const values = Object.values(field.set);
    if (values.some((value) => typeof value !== 'string')) return undefined;
    return values as string[];
  }
  if ((kind === 'scalar' || kind === 'list') && typeof field.value === 'string') return [field.value];
  return undefined;
}

export function configCapability(profile: unknown, appId: string, providerId: string, platform: unknown): ConfigCapability {
  if (!CONFIG_PLATFORMS.some((os) => os === platform) || !isRecord(profile) || profile.id !== appId
    || !Array.isArray(profile.packages) || !profile.packages.some((pkg) => isRecord(pkg) && pkg.kind === 'app'
      && Array.isArray(pkg.platforms) && pkg.platforms.includes(platform))
    || !isRecord(profile.modelProviders) || !Object.prototype.hasOwnProperty.call(profile.modelProviders, providerId)) return 'unavailable';

  const provider = profile.modelProviders[providerId];
  if (!isRecord(provider)) return 'unavailable';
  // The hardened native writer is enabled only on macOS. A listed Windows pair is still usable manually.
  if (platform !== 'macos') return 'manual';
  if (!CONFIG_PROVIDERS.some((id) => id === providerId) || provider.authType !== 'api-key'
    || provider.configWritable !== true) return 'manual';

  const template = provider.configWrite;
  if (!isRecord(template) || typeof template.format !== 'string' || !FORMATS.includes(template.format)
    || !Array.isArray(template.fields) || !template.fields.length) return 'manual';
  const format = template.format;
  const fields = template.fields.map((field) => fieldValues(field, format));
  const supportedPlaceholder = (value: string) => {
    const matches = value.match(/<[^>]*>/g) ?? [];
    return matches.every((placeholder) => ['<apiKey>', '<model>', '<baseUrl>', '<provider>'].includes(placeholder))
      && value.replace(/<[^>]*>/g, '').indexOf('<') === -1
      && value.replace(/<[^>]*>/g, '').indexOf('>') === -1;
  };
  if (fields.some((field) => !field || field.some((value) => !supportedPlaceholder(value)))
    || !fields.some((field) => field?.some((value) => value.includes('<apiKey>')))) return 'manual';

  // Support explicit home-relative catalog targets. Template.file takes precedence.
  const target = Object.prototype.hasOwnProperty.call(template, 'file') ? template.file : provider.configTarget;
  if (typeof target !== 'string' || target.trim() !== target || !target.startsWith('~/')
    || /[\\:%<>\u0000-\u001F\u007F]/.test(target)) return 'manual';
  const parts = target.slice(2).split('/');
  if (parts.some((part) => !part || part === '.' || part === '..')) return 'manual';
  if (parts[0].toLowerCase() === 'appdata') return 'manual';
  return 'assisted';
}
