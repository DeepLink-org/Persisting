"""对 AD 的 search / login 调用封装。"""

from __future__ import annotations

import ssl
from typing import Any

from ldap3 import ALL, SUBTREE, Connection, Server, Tls
from ldap3.core.exceptions import LDAPException

from persisting.ldap_auth.config import LdapAuthConfig

_ATTRS = ["cn", "mail", "sAMAccountName", "userPrincipalName", "memberOf"]


class LdapClient:
    """持有配置与服务账号密码，对外提供 search / login。"""

    def __init__(self, config: LdapAuthConfig, bind_password: str) -> None:
        if not bind_password:
            raise ValueError("bind password is required")
        self._config = config
        self._bind_password = bind_password

    @property
    def default_search_filter(self) -> str:
        """未传 filter 时的默认过滤器。"""
        return self._config.search_filter

    def _server(self) -> Server:
        """构造 LDAP Server（ldaps 时可跳过证书校验）。"""
        use_ssl = self._config.url.lower().startswith("ldaps://")
        tls = None
        if use_ssl:
            validate = ssl.CERT_NONE if self._config.insecure_tls else ssl.CERT_REQUIRED
            tls = Tls(validate=validate, version=ssl.PROTOCOL_TLS_CLIENT)
        return Server(
            self._config.url,
            use_ssl=use_ssl,
            tls=tls,
            get_info=ALL,
            connect_timeout=5,
        )

    def _connect(self, user: str, password: str) -> Connection:
        """连接并 Bind。"""
        return Connection(
            self._server(),
            user=user,
            password=password,
            auto_bind=True,
            raise_exceptions=True,
            receive_timeout=15,
        )

    def search(self, ldap_filter: str | None = None) -> tuple[str, list[dict[str, Any]]]:
        """用服务账号搜索；filter 可与 ldap_demo 相同，例如 (sAMAccountName=zhangbo)。"""
        filt = (ldap_filter or "").strip() or self._config.search_filter
        conn = self._connect(self._config.bind_dn, self._bind_password)
        try:
            try:
                conn.search(
                    search_base=self._config.base_dn,
                    search_filter=filt,
                    search_scope=SUBTREE,
                    attributes=_ATTRS,
                    size_limit=self._config.size_limit,
                )
            except LDAPException:
                # AD 触达 size_limit 时可能已有部分结果
                if not conn.entries:
                    raise
            rows: list[dict[str, Any]] = []
            for entry in conn.entries:
                row: dict[str, Any] = {"dn": entry.entry_dn}
                for name in _ATTRS:
                    if name in entry:
                        value = entry[name].value
                        row[name] = value if not isinstance(value, list) else value
                rows.append(row)
            return filt, rows
        finally:
            conn.unbind()

    def login(self, username: str | None, password: str) -> dict[str, Any]:
        """按用户名搜 DN 后再用密码 Bind；username 默认 config.account。"""
        if not password:
            raise ValueError("password is required")
        account = (username or "").strip() or self._config.account

        user_dn: str | None = None
        mail: str | None = None
        admin = self._connect(self._config.bind_dn, self._bind_password)
        try:
            filt = f"(sAMAccountName={account})"
            admin.search(
                search_base=self._config.base_dn,
                search_filter=filt,
                search_scope=SUBTREE,
                attributes=_ATTRS,
            )
            if admin.entries:
                entry = admin.entries[0]
                user_dn = entry.entry_dn
                mail = entry.mail.value if "mail" in entry else None
        finally:
            admin.unbind()

        # 服务账号等可能不在 Users OU：回退短名 Bind
        identity = user_dn or (self._config.bind_dn if account == self._config.account else None)
        if not identity:
            raise LookupError(f"user not found: {account}")

        user = self._connect(identity, password)
        user.unbind()
        return {
            "ok": True,
            "username": account,
            "dn": identity,
            "mail": mail,
        }

    @staticmethod
    def error_payload(exc: BaseException) -> dict[str, str]:
        """把异常收成可 JSON 返回的结构。"""
        if isinstance(exc, LDAPException):
            return {"error": "ldap_error", "detail": str(exc)}
        if isinstance(exc, LookupError):
            return {"error": "not_found", "detail": str(exc)}
        if isinstance(exc, ValueError):
            return {"error": "bad_request", "detail": str(exc)}
        return {"error": "internal", "detail": str(exc)}
