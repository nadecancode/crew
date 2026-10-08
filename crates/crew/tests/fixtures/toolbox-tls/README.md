# Test TLS material for toolbox MCP servers

Used by `tests/toolbox_proxy.rs` to check that `crew toolbox mcp` reaches an
https server only through a verified chain:

- `ca.pem`: a test CA (`CN=crew toolbox test CA`), valid for 100 years. Its key
  was deleted after signing.
- `server.pem`, `server.key`: a server certificate for `localhost` and
  `127.0.0.1`, signed by that CA, and its PKCS#8 key.

None of this is trusted anywhere else. Regenerate with `openssl req -x509`
(EC P-256, `basicConstraints=critical,CA:TRUE`), then a server request
signed with `subjectAltName=DNS:localhost,IP:127.0.0.1` and
`extendedKeyUsage=serverAuth`.
