# Test TLS identity

A self-signed certificate and its private key, for the server's TLS tests
only. Nothing trusts them outside those tests; they were generated with:

```sh
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
  -days 36500 -nodes -subj "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" \
  -addext "basicConstraints=critical,CA:FALSE" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=serverAuth" \
  -keyout key.pem -out cert.pem
```

It is an end-entity certificate (`CA:FALSE`): rustls refuses a CA
certificate presented as a server's own.
