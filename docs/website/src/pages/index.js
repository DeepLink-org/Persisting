import React from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';
import useBaseUrl from '@docusaurus/useBaseUrl';
import CodeBlock from '@theme/CodeBlock';
import clsx from 'clsx';
import styles from './index.module.css';

const capabilities = [
  ['Governed execution', 'Run an Agent inside a provider-aware, reviewable workspace.'],
  ['Reviewable changes', 'Inspect a complete stage before any file reaches the project.'],
  ['Capability evidence', 'See which filesystem, network, and process controls actually applied.'],
  ['Trajectory Datasets', 'Turn captured or imported history into normalized, queryable views.'],
  ['Open formats', 'Exchange supported trajectory formats while preserving Source lineage.'],
  ['Local first', 'Start locally and move to object storage when the workflow is ready.'],
];

export default function Home() {
  const productDiagram = useBaseUrl('/img/diagrams/persisting/system-products.svg');
  return <Layout title="Persistent Infrastructure for the Agent Era" description="Run Agents under a reviewable execution boundary and preserve queryable history.">
    <header className={clsx('hero hero--primary', styles.hero)}><div className="container">
      <p className={styles.eyebrow}>PERSISTENT INFRASTRUCTURE FOR AGENTS</p>
      <h1 className="hero__title">Persistent Infrastructure for the Agent Era</h1>
      <p className="hero__subtitle">Run an Agent with a reviewable execution boundary. Preserve the resulting history as a queryable Dataset.</p>
      <div className="buttons"><Link className="button button--secondary button--lg" to="/docs/">Get started</Link><Link className="button button--outline button--lg" to="/docs/system-design/">Read the system design</Link></div>
    </div></header>
    <main>
      <section className="container padding-vert--xl"><div className="row product-choices">
        <div className="col col--6"><article className="product-card"><p className="product-kicker">GOVERNED EXECUTION</p><h2>pVisor</h2><p>Run one Agent in a staged workspace and a selected execution provider. Inspect Evidence, then apply or discard Effects.</p><p className="product-outcome">You finish with a reviewable Run Bundle and controlled project changes.</p><Link to="/docs/pvisor/">Start a Run →</Link></article></div>
        <div className="col col--6"><article className="product-card"><p className="product-kicker">DURABLE HISTORY</p><h2>pChronicle</h2><p>Pin, normalize, query, and exchange Agent trajectory Sources. Browse durable history without requiring a pVisor Run.</p><p className="product-outcome">You finish with a queryable Dataset and traceable Source lineage.</p><Link to="/docs/pchronicle/">Explore Datasets →</Link></article></div>
      </div></section>
      <section className="container padding-vert--xl architecture-section"><div className="text--center"><h2>One boundary, two durable paths</h2><p className="section-lede">Execution and history stay independently useful. Connect them only when the workflow needs a durable handoff.</p></div><img className="architecture-diagram" src={productDiagram} alt="pVisor execution and pChronicle history paths" /><div className="row workflow-steps"><div className="col col--4"><span className="step-number">01</span><h3><Link to="/docs/pvisor/get-started/">Run</Link></h3><p>Give the Agent a staged workspace and record the effective controls.</p></div><div className="col col--4"><span className="step-number">02</span><h3><Link to="/docs/pvisor/guides/review-apply/">Review</Link></h3><p>Inspect Evidence and Effects before deciding what crosses into the project.</p></div><div className="col col--4"><span className="step-number">03</span><h3><Link to="/docs/pchronicle/get-started/">Remember</Link></h3><p>Capture selected lifecycle facts and trajectory events as a queryable Dataset.</p></div></div></section>
      <section className="container padding-vert--lg"><h2>Everything needed for a durable Agent workflow</h2><div className="quickstart-grid">{capabilities.map(([title, body]) => <article className="quickstart-card" key={title}><h3>{title}</h3><p>{body}</p></article>)}</div></section>
      <section className="container padding-vert--xl quickstart-section"><div className="text--center"><h2>From install to a useful result</h2><p className="section-lede">Run the smallest complete path first. Keep the output, then choose the guide that answers your next question.</p></div><div className="quickstart-commands"><article><span className="step-number">01</span><h3>Install</h3><CodeBlock language="bash">{"pip install 'persisting[lance]'"}</CodeBlock></article><article><span className="step-number">02</span><h3>Run or explore</h3><CodeBlock language="bash">{"pvisor run --stage ./runs/task-001 -- codex\n# or: pchronicle onboard"}</CodeBlock></article><article><span className="step-number">03</span><h3>Review or query</h3><CodeBlock language="bash">{"pvisor review last\n# or: pchronicle onboard query"}</CodeBlock></article></div><div className="text--center margin-top--lg"><div className="buttons"><Link className="button button--primary" to="/docs/pvisor/get-started/">Run your first Agent</Link><Link className="button button--secondary" to="/docs/pchronicle/get-started/">Explore a Dataset</Link></div></div></section>
    </main>
  </Layout>;
}
