"""从本地 JSON 加载 LDAP 连接配置（不含密码）。"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class LdapAuthConfig:
    """本地配置文件中的字段；bind 密码仅允许启动参数传入。"""

    host: str
    url: str
    base_dn: str
    bind_dn: str
    account: str
    insecure_tls: bool
    size_limit: int

    @property
    def search_filter(self) -> str:
        """未指定 filter 时的默认搜索条件。"""
        return f"(sAMAccountName={self.account})"

    @classmethod
    def load(cls, path: str | Path) -> LdapAuthConfig:
        """读取 JSON 配置文件。"""
        data = json.loads(Path(path).read_text(encoding="utf-8"))
        required = ("url", "base_dn", "bind_dn")
        missing = [key for key in required if not str(data.get(key, "")).strip()]
        if missing:
            raise ValueError(f"config missing fields: {', '.join(missing)}")
        return cls(
            host=str(data.get("host", "0.0.0.0")).strip() or "0.0.0.0",
            url=str(data["url"]).strip(),
            base_dn=str(data["base_dn"]).strip(),
            bind_dn=str(data["bind_dn"]).strip(),
            account=str(data.get("account", "persisting")).strip() or "persisting",
            insecure_tls=bool(data.get("insecure_tls", True)),
            size_limit=int(data.get("size_limit", 20)),
        )
