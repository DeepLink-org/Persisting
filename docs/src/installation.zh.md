# 安装指南

## 环境要求

- Python 3.10+
- Pulsing（作为依赖自动安装）

## 安装

```bash
# 推荐：带 Lance 支持
pip install persisting[lance]

# 最小安装（不含 Lance，仅用于自定义后端）
pip install persisting
```

### 从源码安装

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
pip install -e ".[lance]"
```

### Nightly（预发布 wheel）

每日 / `main` 推送会滚动更新 GitHub Release 标签 [`nightly`](https://github.com/DeepLink-org/Persisting/releases/tag/nightly)：

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

## 验证

```python
import persisting
print(persisting.__version__)

from persisting.queue import LanceBackend, PersistingBackend
print("LanceBackend:", LanceBackend)
print("PersistingBackend:", PersistingBackend)
```

## 依赖

| 包名 | 版本 | 必需 | 说明 |
|------|------|------|------|
| `pulsing` | >=0.1.0 | 是 | 分布式 actor 运行时（控制面） |
| `lance` | >=0.9.0 | 可选 (`[lance]`) | Lance 列式存储 |
| `pyarrow` | >=14.0.0 | 可选 (`[lance]`) | Apache Arrow |

## 下一步

- [快速开始](quickstart.md) — 5 分钟上手
- [API 参考](api_reference.md) — 完整 API 文档
