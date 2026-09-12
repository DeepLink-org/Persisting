import React from 'react';
import Link from '@docusaurus/Link';
import Tabs from '@theme/Tabs';
import TabItem from '@theme/TabItem';
import CodeBlock from '@theme/CodeBlock';

export default function HomeQuickstart({chinese = false}) {
  const docs = chinese ? '/zh/docs' : '/docs';
  return (
    <div className="home-terminal">
      <Tabs aria-label={chinese ? '快速开始命令' : 'Quick start commands'}>
        <TabItem value="install" label={chinese ? '安装' : 'Install'}>
          <CodeBlock language="bash" title={chinese ? '安装 Persisting' : 'Install Persisting'}>{'pip install persisting'}</CodeBlock>
          <Link to={`${docs}/installation`}>{chinese ? '查看安装指南 →' : 'Installation guide →'}</Link>
        </TabItem>
        <TabItem value="explore" label={chinese ? '探索 Dataset' : 'Explore a Dataset'}>
          <CodeBlock language="bash" title="pChronicle">{'pchronicle onboard'}</CodeBlock>
          <Link to={`${docs}/pchronicle/get-started/`}>{chinese ? '探索第一个 Dataset →' : 'Explore your first Dataset →'}</Link>
        </TabItem>
        <TabItem value="run" label={chinese ? '运行 Agent' : 'Run an Agent'}>
          <CodeBlock language="bash" title="pVisor">{'pvisor run --stage ./runs/task-001 -- codex'}</CodeBlock>
          <Link to={`${docs}/pvisor/get-started/`}>{chinese ? '运行第一个 Agent →' : 'Run your first Agent →'}</Link>
        </TabItem>
      </Tabs>
    </div>
  );
}
