# wyvrn-hosted-did-wasm

[`wyvrn-hosted-did`](../wyvrn-hosted-did) for a web app: its `api` module (text in,
text out) as a WebAssembly module.

```sh
wasm-pack build --target web --out-dir pkg-web --release
```

`pkg-web/` is what an app depends on (`"wyvrn-hosted-did-wasm": "file:…/pkg-web"`).
It is built, not committed. Every function takes and gives JSON text; see
`wyvrn_hosted_did::api` for each. A native app calls that module directly instead.
