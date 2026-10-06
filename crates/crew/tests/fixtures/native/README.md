# Native TLS test identity

`cert.pem` and `key.pem` are synthetic public test fixtures. The private key
is deliberately public, generated solely for isolated loopback regression
tests; it must never be used for a deployed gateway or an operator identity.
The certificate names only `127.0.0.1` and `localhost`.

Tests copy the fixture key into a temporary private runtime/native directory
with mode 0600. Production key loading rejects this original fixture path,
any path outside the protected runtime/native tree, symlinks and hard links.
No test listener exposes this identity to the LAN.
