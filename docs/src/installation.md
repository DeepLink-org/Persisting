# Installation

## Requirements

- Python 3.10+
- Pulsing (installed automatically as dependency)

## Install

```bash
# Recommended: with Lance support
pip install persisting[lance]

# Minimal (without Lance — for custom backends only)
pip install persisting
```

### From Source

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
pip install -e ".[lance]"
```

### Nightly (pre-release wheels)

Daily / `main` pushes refresh the rolling GitHub Release tag [`nightly`](https://github.com/DeepLink-org/Persisting/releases/tag/nightly):

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

## Verify

```python
import persisting
print(persisting.__version__)

from persisting.queue import LanceBackend, PersistingBackend
print("LanceBackend:", LanceBackend)
print("PersistingBackend:", PersistingBackend)
```

## Dependencies

| Package | Version | Required | Description |
|---------|---------|----------|-------------|
| `pulsing` | >=0.1.0 | Yes | Distributed actor runtime (control plane) |
| `lance` | >=0.9.0 | Optional (`[lance]`) | Lance columnar storage |
| `pyarrow` | >=14.0.0 | Optional (`[lance]`) | Apache Arrow |

## Next Steps

- [Quick Start](quickstart.md) — Get started in 5 minutes
- [API Reference](api_reference.md) — Full API documentation
