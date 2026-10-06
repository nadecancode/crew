# Isolated gateway test identity

These certificate/key fixtures are exact copies of the reviewed runtime
gateway fixtures at `759aea95`. The key is deliberately public and synthetic;
it must never be used for a deployed gateway or an operator identity. The
certificate names only `127.0.0.1` and `localhost`. Tests bind ephemeral
loopback listeners and copy the key into a temporary private runtime directory.

`wrong-name.pem` uses that same synthetic key and names only
`wrong-name.example`. It exercises hostname refusal before any device proof.
