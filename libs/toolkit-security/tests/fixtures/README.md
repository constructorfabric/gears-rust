These are public, test-only ECDSA P-256 fixtures. Never use the private key in a
real deployment. The same key is deliberately included in local/E2E configs.

The PKCS#8 key and independent DER signature were generated with OpenSSL:

```sh
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out test-key.pem
openssl pkcs8 -topk8 -nocrypt -in test-key.pem -outform DER -out p256-test-key.pk8
openssl dgst -sha256 -sign test-key.pem -out p256-signature.der p256-message.txt
```
