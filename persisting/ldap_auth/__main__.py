"""python -m persisting.ldap_auth --config ... --port ... --password ..."""

from persisting.ldap_auth.server import main

if __name__ == "__main__":
    raise SystemExit(main())
