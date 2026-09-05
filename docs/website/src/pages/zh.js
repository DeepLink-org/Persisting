import React from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';
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
  return <Layout title="Agent 时代的持久化基础设施" description="在可审查的执行边界中运行 Agent，并保存可查询历史。">
    <header className={`hero hero--primary ${styles.hero}`}><div className="container">
      <p className={styles.eyebrow}>面向 AGENT 的持久化基础设施</p>
      <h1 className="hero__title">Agent 时代的持久化基础设施</h1>
      <p className="hero__subtitle">在可审查的执行边界中运行 Agent，并将产生的历史保存为可查询 Dataset。</p>
      <div className="buttons"><Link className="button button--secondary button--lg" to="/zh/docs/">开始使用</Link><Link className="button button--outline button--lg" to="/zh/docs/system-design/">阅读系统设计</Link></div>
      <img className="hero-cyberpunk" src="/Persisting/img/diagrams/persisting/hero-cyberpunk.svg" alt="Persisting 执行与历史架构" />
    </div></header>
    <main>
      <section className="container padding-vert--xl"><div className="row">
        <div className="col col--6"><h2>pVisor</h2><p>在 staged workspace 与选定执行 Provider 中治理单个 Agent Run。查看 Evidence，再决定 apply 或 drop Effect。</p><Link to="/zh/docs/pvisor/">开始一次 Run →</Link></div>
        <div className="col col--6"><h2>pChronicle</h2><p>固定、规范化、查询和交换 Agent 轨迹 Source。无需先运行 pVisor，也可以浏览持久历史。</p><Link to="/zh/docs/pchronicle/">探索 Dataset →</Link></div>
      </div></section>
      <section className="container padding-vert--xl architecture-section"><div className="text--center"><h2>一条边界，两条持久化路径</h2><p className="section-lede">执行与历史各自独立可用，只有在工作流需要持久交接时才把它们连接起来。</p></div><img className="architecture-diagram" src="/Persisting/img/diagrams/persisting/system-products.svg" alt="pVisor 执行路径与 pChronicle 历史路径" /><div className="row workflow-steps"><div className="col col--4"><span className="step-number">01</span><h3>运行</h3><p>pVisor 为 Agent 提供 staged workspace，并记录实际生效的控制机制。</p></div><div className="col col--4"><span className="step-number">02</span><h3>审查</h3><p>在决定哪些修改进入项目之前，先检查 Evidence 与 Effect。</p></div><div className="col col--4"><span className="step-number">03</span><h3>留存</h3><p>把选定的生命周期事实与轨迹事件捕获为可查询 Dataset。</p></div></div></section>
      <section className="container padding-vert--lg"><h2>一条持久化 Agent 工作流所需的一切</h2><div className="quickstart-grid">{capabilities.map(([title, body]) => <article className="quickstart-card" key={title}><h3>{title}</h3><p>{body}</p></article>)}</div></section>
      <section className="hero hero--dark"><div className="container text--center padding-vert--xl"><h2>从安装到得到结果</h2><p>选择一条路径，先完成最小闭环；只有在需要理解边界时再深入阅读。</p><div className="buttons"><Link className="button button--primary" to="/zh/docs/pvisor/get-started/">运行第一个 Agent</Link><Link className="button button--secondary" to="/zh/docs/pchronicle/get-started/">探索一个 Dataset</Link></div></div></section>
    </main>
  </Layout>;
}
