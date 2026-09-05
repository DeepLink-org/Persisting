import React from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';
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
  return <Layout title="Persistent Infrastructure for the Agent Era" description="Run Agents under a reviewable execution boundary and preserve queryable history.">
    <header className={clsx('hero hero--primary', styles.hero)}><div className="container">
      <p className={styles.eyebrow}>PERSISTENT INFRASTRUCTURE FOR AGENTS</p>
      <h1 className="hero__title">Persistent Infrastructure for the Agent Era</h1>
      <p className="hero__subtitle">Run an Agent with a reviewable execution boundary. Preserve the resulting history as a queryable Dataset.</p>
      <div className="buttons"><Link className="button button--secondary button--lg" to="/docs/">Get started</Link><Link className="button button--outline button--lg" to="/docs/system-design/">Read the system design</Link></div>
      <img className="hero-cyberpunk" src="/Persisting/img/diagrams/persisting/hero-cyberpunk.svg" alt="Persisting execution and history architecture" />
    </div></header>
    <main>
      <section className="container padding-vert--xl"><div className="row">
        <div className="col col--6"><h2>pVisor</h2><p>Govern one Agent Run across a staged workspace and a selected execution provider. Inspect Evidence, then apply or discard Effects.</p><Link to="/docs/pvisor/">Start a Run →</Link></div>
        <div className="col col--6"><h2>pChronicle</h2><p>Pin, normalize, query, and exchange Agent trajectory Sources. Browse durable history without requiring a pVisor Run.</p><Link to="/docs/pchronicle/">Explore Datasets →</Link></div>
      </div></section>
      <section className="container padding-vert--lg"><h2>Everything needed for a durable Agent workflow</h2><div className="quickstart-grid">{capabilities.map(([title, body]) => <article className="quickstart-card" key={title}><h3>{title}</h3><p>{body}</p></article>)}</div></section>
      <section className="hero hero--dark"><div className="container text--center padding-vert--xl"><h2>From install to a useful result</h2><p>Choose one path, complete the smallest workflow, then go deeper when the boundary matters.</p><div className="buttons"><Link className="button button--primary" to="/docs/pvisor/get-started/">Run your first Agent</Link><Link className="button button--secondary" to="/docs/pchronicle/get-started/">Explore a Dataset</Link></div></div></section>
    </main>
  </Layout>;
}
