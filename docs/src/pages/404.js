import React from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';
import clsx from 'clsx';
import styles from './index.module.css';

export default function NotFound() {
  return (
    <Layout title="Page not found" description="The requested Persisting documentation page was not found.">
      <main className={clsx('hero hero--primary', styles.hero)}>
        <div className="container">
          <p className={styles.eyebrow}>404 / PAGE NOT FOUND</p>
          <h1 className="hero__title">The path changed. The workflow did not.</h1>
          <p className="hero__subtitle">
            Return to the documentation map, search the published content, or start
            with one of Persisting's two product paths.
          </p>
          <div className="buttons">
            <Link className="button button--secondary button--lg" to="/docs/">Start here</Link>
            <Link className="button button--outline button--lg" to="/search/">Search docs</Link>
            <Link className="button button--outline button--lg" to="/docs/pvisor/">pVisor</Link>
            <Link className="button button--outline button--lg" to="/docs/pchronicle/">pChronicle</Link>
          </div>
        </div>
      </main>
    </Layout>
  );
}
