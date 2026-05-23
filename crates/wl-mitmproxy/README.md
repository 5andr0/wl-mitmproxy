# wl-mitmproxy

Default install/builds enable `suite-wayland-protocols` from `wl-proxy`.

Runtime logging is controlled with `RUST_LOG`. By default only errors are emitted; set `RUST_LOG=info` to see normal proxy status messages.

To select a different forwarded feature set:

```bash
cargo install wl-mitmproxy --no-default-features --features suite-weston-protocols
```

Full documentation, usage examples, and project details:

https://github.com/5andr0/wl-mitmproxy

## License

This project is licensed under either of

    Apache License, Version 2.0
    MIT License

at your option.