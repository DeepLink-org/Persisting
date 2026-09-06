import React from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';
import useBaseUrl from '@docusaurus/useBaseUrl';
import CodeBlock from '@theme/CodeBlock';
import styles from './index.module.css';

const capabilities = [
  ['受治理执行', '在了解 Provider 边界、可审查的 workspace 中运行 Agent。'],
  ['可审查修改', '在文件进入真实项目之前，先检查完整 stage。'],
  ['Capability Evidence', '查看实际生效的文件系统、网络和进程控制。'],
  ['轨迹 Dataset', '把捕获或导入的历史转换为规范化、可查询的视图。'],
  ['开放格式', '交换支持的轨迹格式，同时保留 Source lineage。'],
  ['本地优先', '从本地路径开始，准备好后再迁移到对象存储。'],
];

export default function ChineseHome() {
  const productDiagram = useBaseUrl('/img/diagrams/persisting/system-products.svg');
  return <Layout title="Agent 时代的持久化基础设施" description="在可审查的执行边界中运行 Agent，并保存可查询历史。">
    <header className={`hero hero--primary ${styles.hero}`}><div className="container">
      <p className={styles.eyebrow}>面向 AGENT 的持久化基础设施</p>
      <h1 className="hero__title">Agent 时代的持久化基础设施</h1>
      <p className="hero__subtitle">在可审查的执行边界中运行 Agent，保留经过确认的决策，并将产生的历史保存为可查询 Dataset。</p>
      <div className="buttons"><Link className="button button--secondary button--lg" to="/zh/docs/">开始使用</Link><Link className="button button--outline button--lg" to="/zh/docs/system-design/">阅读系统设计</Link></div>
    </div></header>
    <main>
      <section className="container padding-vert--xl"><div className="row product-choices">
        <div className="col col--6"><article className="product-card"><p className="product-kicker">受治理执行</p><h2>pVisor</h2><p>为 Agent 提供 staged workspace 与明确的执行 Provider。在 Effect 进入项目之前先查看 Evidence。</p><p className="product-outcome">最终得到可审查的 Run Bundle，并控制哪些修改进入项目。</p><Link to="/zh/docs/pvisor/">开始一次 Run →</Link></article></div>
        <div className="col col--6"><article className="product-card"><p className="product-kicker">持久历史</p><h2>pChronicle</h2><p>固定、规范化、查询和交换 Agent 轨迹 Source，无论它是否来自 pVisor。</p><p className="product-outcome">最终得到可查询 Dataset，并保留清晰的 Source lineage。</p><Link to="/zh/docs/pchronicle/">探索 Dataset →</Link></article></div>
      </div></section>
      <section className="container padding-vert--xl architecture-section"><div className="text--center"><h2>一条边界，两条持久化路径</h2><p className="section-lede">执行与历史各自独立可用，只有在工作流需要持久交接时才把它们连接起来。</p></div><img className="architecture-diagram" src={productDiagram} alt="pVisor 执行路径与 pChronicle 历史路径" /><div className="row workflow-steps"><div className="col col--4"><span className="step-number">01</span><h3><Link to="/zh/docs/pvisor/get-started/">运行</Link></h3><p>为 Agent 提供 staged workspace，并记录实际生效的控制机制。</p></div><div className="col col--4"><span className="step-number">02</span><h3><Link to="/zh/docs/pvisor/guides/review-apply/">审查</Link></h3><p>在决定哪些修改进入项目之前，先检查 Evidence 与 Effect。</p></div><div className="col col--4"><span className="step-number">03</span><h3><Link to="/zh/docs/pchronicle/get-started/">留存</Link></h3><p>把选定的生命周期事实与轨迹事件捕获为可查询 Dataset。</p></div></div></section>
      <section className="container padding-vert--lg"><h2>一条持久化 Agent 工作流所需的一切</h2><div className="quickstart-grid">{capabilities.map(([title, body]) => <article className="quickstart-card" key={title}><h3>{title}</h3><p>{body}</p></article>)}</div></section>
      <section className="container padding-vert--xl quickstart-section"><div className="text--center"><h2>从安装到得到结果</h2><p className="section-lede">先完成最小闭环，保留输出，再按下一个问题选择深入指南。</p></div><div className="quickstart-commands"><article><span className="step-number">01</span><h3>安装</h3><CodeBlock language="bash">{"pip install persisting"}</CodeBlock></article><article><span className="step-number">02</span><h3>运行或探索</h3><CodeBlock language="bash">{"pvisor run --stage ./runs/task-001 -- codex\n# 或：pchronicle onboard"}</CodeBlock></article><article><span className="step-number">03</span><h3>审查或查询</h3><CodeBlock language="bash">{"pvisor review last\n# 或：pchronicle onboard query"}</CodeBlock></article></div><div className="text--center margin-top--lg"><div className="buttons"><Link className="button button--primary" to="/zh/docs/pvisor/get-started/">运行第一个 Agent</Link><Link className="button button--secondary" to="/zh/docs/pchronicle/get-started/">探索一个 Dataset</Link></div></div></section>
    </main>
  </Layout>;
}
