# Installation

This guide covers the installation of Persisting.

## Requirements

- Python 3.10 or higher
- pip or uv package manager

## Installation Methods

### Using pip

```bash
# Basic installation
pip install persisting

# With Pulsing integration
pip install persisting[pulsing]

# With all optional dependencies
pip install persisting[all]
```

### Using uv

```bash
# Basic installation
uv pip install persisting

# With Pulsing integration
uv pip install persisting[pulsing]
```

### From Source

```bash
# Clone the repository
git clone https://github.com/reiase/Persisting.git
cd Persisting

# Install in development mode
pip install -e .

# Or with uv
uv pip install -e .
```

## Dependencies

Persisting has the following dependencies:

| Package | Version | Description |
|---------|---------|-------------|
| `lance` | >=0.9.0 | Lance columnar data format |
| `pyarrow` | >=14.0.0 | Apache Arrow for data interchange |
| `pulsing` | >=0.1.0 | (Optional) Pulsing Actor framework |

## Verification

Verify your installation:

```python
import persisting
print(persisting.__version__)

# Check available backends
from persisting.queue import LanceBackend, PersistingBackend
print("LanceBackend available:", LanceBackend is not None)
print("PersistingBackend available:", PersistingBackend is not None)
```

## Next Steps

- [Quick Start](quickstart.md) - Get started with Persisting
- [User Guide](guide/index.md) - Learn about storage backends
- [API Reference](api_reference.md) - Detailed API documentation

