/**
 * S3 PLAN 依赖链 DAG：HTML+CSS 网格，不引图表库。
 * 节点三色：--ok=已满足(复用) / --info=将安装 / 灰=无需；边=1px --line 竖线。
 * 同时承载 CONFIRM 态（计划确认：将装什么/占多少磁盘/预计时长 + 开始安装）。
 */
import { Fragment } from "react";
import { useTranslation } from "react-i18next";
import type { Plan, PlanNode } from "../state";
import type { Status } from "../state";

function nodeClass(kind: PlanNode["kind"]): string {
  switch (kind) {
    case "target":
    case "install":
      return "border-aibox-info text-aibox-info";
    case "reuse":
      return "border-aibox-ok text-aibox-ok";
    case "skip":
      return "border-aibox-line text-aibox-text2";
  }
}

function Node({ node, target }: { node: PlanNode; target: boolean }) {
  return (
    <div
      className={[
        "rounded-[4px] border bg-aibox-raised px-3 py-2 text-center",
        nodeClass(node.kind),
        target ? "font-display text-sm font-semibold" : "font-mono text-xs",
      ].join(" ")}
    >
      <div className="text-aibox-text1">{node.name}</div>
      <div className="font-mono text-[10px] text-aibox-text2">{node.versionRange}</div>
    </div>
  );
}

export default function S3Plan({
  plan,
  status,
}: {
  plan: Plan | null;
  status: Status;
}) {
  const { t } = useTranslation();
  if (!plan) return <p className="font-mono text-xs text-aibox-text2">{t("s3.building")}</p>;

  const roots = plan.nodes.filter((n) => !plan.nodes.some((p) => p.children.includes(n.id)));

  return (
    <div className="flex h-full flex-col gap-5">
      <h1 className="font-display text-xl font-bold text-aibox-cream">{t("s3.title")}</h1>

      {/* 图例 */}
      <div className="flex gap-4 font-mono text-[11px]">
        <span className="text-aibox-ok">● {t("s3.legendReuse")}</span>
        <span className="text-aibox-info">● {t("s3.legendInstall")}</span>
        <span className="text-aibox-text2">● {t("s3.legendSkip")}</span>
      </div>

      {/* DAG：自上而下，层间 1px 竖线 */}
      <div className="flex min-h-0 flex-1 flex-col items-center gap-0 overflow-y-auto">
        {roots.map((root) => (
          <div key={root.id} className="flex flex-col items-center">
            <Node node={root} target />
            {root.children.map((cid) => {
              const child = plan.nodes.find((n) => n.id === cid);
              if (!child) return null;
              return (
                <Fragment key={cid}>
                  <span className="h-5 w-px bg-aibox-line" aria-hidden />
                  <Node node={child} target={false} />
                </Fragment>
              );
            })}
          </div>
        ))}
      </div>

      {/* CONFIRM：计划确认信息 + 开始安装 */}
      {status === "CONFIRM" && (
        <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
          <h2 className="mb-2 font-display text-sm font-semibold text-aibox-cream">
            {t("s3.confirmTitle")}
          </h2>
          <dl className="mb-3 grid grid-cols-2 gap-x-6 gap-y-1 font-mono text-xs sm:grid-cols-4">
            <div>
              <dt className="text-aibox-text2">{t("s3.route")}</dt>
              <dd className="text-aibox-text1">{plan.route}</dd>
            </div>
            <div>
              <dt className="text-aibox-text2">{t("s3.channel")}</dt>
              <dd className="text-aibox-text1">{plan.channel}</dd>
            </div>
            <div>
              <dt className="text-aibox-text2">{t("s3.disk")}</dt>
              <dd className="text-aibox-text1">{plan.diskMb} MB</dd>
            </div>
            <div>
              <dt className="text-aibox-text2">{t("s3.est")}</dt>
              <dd className="text-aibox-text1">~{plan.estMinutes} min</dd>
            </div>
          </dl>
          <p className="text-xs text-aibox-text2">{t("s3.confirmNote")}</p>
        </section>
      )}
    </div>
  );
}
