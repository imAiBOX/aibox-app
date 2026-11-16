/**
 * C2「去哪弄 key」分步矢量示意图（程序化 SVG，零外部素材、永不过期）。
 * 风格遵循 VI：黑底 #0A0A0A、主绿 #B6FB50 高亮、米白文字、像素方块母题、无渐变。
 * step 1 = 打开官网注册 · step 2 = 创建并复制 key · step 3 = 粘贴进 AiBOX
 */
interface Props {
  step: 1 | 2 | 3;
  domain?: string;
  /** 无障碍标题（i18n 文本） */
  title?: string;
}

const BG = "#0A0A0A";
const LINE = "#2A2E33";
const GREEN = "#B6FB50";
const CREAM = "#FBF6E2";
const TEXT2 = "#9BA3AF";

function BrowserFrame({ domain, children }: { domain: string; children: React.ReactNode }) {
  return (
    <g>
      <rect x="8" y="8" width="300" height="144" rx="4" fill={BG} stroke={LINE} strokeWidth="2" />
      <rect x="20" y="18" width="8" height="8" rx="2" fill={TEXT2} />
      <rect x="34" y="18" width="8" height="8" rx="2" fill={TEXT2} />
      <rect x="48" y="18" width="8" height="8" rx="2" fill={GREEN} />
      <rect x="64" y="16" width="232" height="12" rx="3" fill="#14161A" stroke={LINE} />
      <text x="72" y="25" fontSize="8" fill={TEXT2} fontFamily="monospace">{domain}</text>
      {children}
    </g>
  );
}

export default function KeyStepDiagram({ step, domain = "provider.example", title }: Props) {
  return (
    <svg
      viewBox="0 0 480 160"
      role="img"
      aria-label={title ?? `step ${step}`}
      className="w-full max-w-[420px] rounded-[4px] border border-aibox-line"
      style={{ background: BG }}
    >
      {step === 1 && (
        <BrowserFrame domain={domain}>
          <rect x="28" y="44" width="140" height="14" rx="3" fill="#14161A" stroke={LINE} />
          <rect x="28" y="66" width="140" height="14" rx="3" fill="#14161A" stroke={LINE} />
          <rect x="28" y="92" width="72" height="18" rx="3" fill={GREEN} />
          <text x="40" y="105" fontSize="9" fill={BG} fontWeight="700">Sign up</text>
          <rect x="22" y="38" width="156" height="78" rx="4" fill="none" stroke={GREEN} strokeWidth="2" strokeDasharray="6 4" />
          <text x="192" y="80" fontSize="11" fill={CREAM}>1</text>
          <text x="192" y="96" fontSize="9" fill={TEXT2}>register</text>
        </BrowserFrame>
      )}
      {step === 2 && (
        <BrowserFrame domain={domain}>
          <rect x="28" y="48" width="200" height="16" rx="3" fill="#14161A" stroke={LINE} />
          <text x="36" y="60" fontSize="10" fill={GREEN} fontFamily="monospace">sk-••••••••••••••••</text>
          <rect x="236" y="48" width="56" height="16" rx="3" fill={GREEN} />
          <text x="248" y="60" fontSize="9" fill={BG} fontWeight="700">Copy</text>
          <rect x="22" y="42" width="276" height="28" rx="4" fill="none" stroke={GREEN} strokeWidth="2" strokeDasharray="6 4" />
          <text x="28" y="92" fontSize="9" fill={TEXT2}>create key → copy</text>
        </BrowserFrame>
      )}
      {step === 3 && (
        <g>
          <rect x="8" y="8" width="300" height="144" rx="4" fill={BG} stroke={LINE} strokeWidth="2" />
          <rect x="8" y="8" width="28" height="144" fill="#14161A" stroke={LINE} />
          <rect x="16" y="24" width="12" height="12" rx="3" fill={GREEN} />
          <rect x="16" y="44" width="12" height="12" rx="3" fill={LINE} />
          <rect x="16" y="64" width="12" height="12" rx="3" fill={LINE} />
          <rect x="52" y="44" width="200" height="36" rx="4" fill="#14161A" stroke={GREEN} strokeWidth="2" />
          <text x="62" y="66" fontSize="10" fill={CREAM} fontFamily="monospace">sk-••••••••••••</text>
          <text x="266" y="66" fontSize="16" fill={GREEN} fontWeight="700">✓</text>
          <text x="52" y="104" fontSize="9" fill={TEXT2}>paste into AiBOX → verify</text>
        </g>
      )}
    </svg>
  );
}
