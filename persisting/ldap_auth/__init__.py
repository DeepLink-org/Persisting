"""本机 LDAP 验证小服务：POST /search、POST /login。"""

from __future__ import annotations

__all__ = ["LdapAuthConfig", "LdapClient", "serve", "main"]

from persisting.ldap_auth.client import LdapClient
from persisting.ldap_auth.config import LdapAuthConfig
from persisting.ldap_auth.server import main, serve
