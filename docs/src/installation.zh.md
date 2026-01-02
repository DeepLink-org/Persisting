# 安装指南

本指南介绍如何安装 Persisting。

## 环境要求

- Python 3.10 或更高版本
- pip 或 uv 包管理器

## 安装方式

### 使用 pip

```bash
# 基础安装
pip install persisting

# 包含 Pulsing 集成
pip install persisting[pulsing]

# 包含所有可选依赖
pip install persisting[all]
```

### 使用 uv

```bash
# 基础安装
uv pip install persisting

# 包含 Pulsing 集成
uv pip install persisting[pulsing]
```

### 从源码安装

```bash
# 克隆仓库
git clone https://github.com/reiase/Persisting.git
cd Persisting

# 开发模式安装
pip install -e .

# 或使用 uv
uv pip install -e .
```

## 依赖项

Persisting 有以下依赖：

| 包名 | 版本 | 说明 |
|------|------|------|
| `lance` | >=0.9.0 | Lance 列式数据格式 |
| `pyarrow` | >=14.0.0 | Apache Arrow 数据交换 |
| `pulsing` | >=0.1.0 | (可选) Pulsing Actor 框架 |

## 验证安装

验证安装是否成功：

```python
import persisting
print(persisting.__version__)

# 检查可用后端
from persisting.queue import LanceBackend, PersistingBackend
print("LanceBackend 可用:", LanceBackend is not None)
print("PersistingBackend 可用:", PersistingBackend is not None)
```

## 下一步

- [快速开始](quickstart.md) - 开始使用 Persisting
- [用户指南](guide/index.md) - 了解存储后端
- [API 参考](api_reference.md) - 详细 API 文档

