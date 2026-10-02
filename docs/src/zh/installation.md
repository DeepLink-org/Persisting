# 安装

本仓库发布 `pchronicle` 命令和内嵌 Web UI。

## 安装

```bash
pip install persisting
pchronicle --version
pchronicle onboard
```

平台 wheel 支持 Linux x86_64 和 Apple Silicon macOS，需要 Python 3.10+。
Python 包与 `pchronicle` 会安装到当前 Python 环境中。

## Nightly 与源码构建

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

在源码目录中，`pip install -e .` 会构建 pChronicle 和 Web 资源。
`just install-cli` 安装 Rust CLI；构建内嵌 Web 资源需要 Dioxus CLI。
参见[工程说明](project/engineering.md)。

继续阅读[探索第一个 Dataset](pchronicle/get-started.md)或
