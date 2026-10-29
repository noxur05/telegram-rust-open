# Deployment

## SSH Proxy

All SSH/SCP commands require a SOCKS5 proxy on port 2080:
```
-o ProxyCommand='nc -X 5 -x 127.0.0.1:2080 %h %p'
```

## Steps

1. Build the release binary:
   ```bash
   cargo build --release
   ```

2. Stop the service (binary is locked while running):
   ```bash
   ssh -o ProxyCommand='nc -X 5 -x 127.0.0.1:2080 %h %p' root@example-a.com "systemctl stop telegram-bot.service"
   ```

3. Copy the binary to the server:
   ```bash
   scp -o ProxyCommand='nc -X 5 -x 127.0.0.1:2080 %h %p' target/release/telegram-happ-rust root@example-a.com:~/rust/telegram-happ-rust
   ```

4. Restart the service:
   ```bash
   ssh -o ProxyCommand='nc -X 5 -x 127.0.0.1:2080 %h %p' root@example-a.com "systemctl restart telegram-bot.service"
   ```

## Server Details

- **Host:** `root@example-a.com`
- **Binary location:** `~/rust/telegram-happ-rust`
- **Service name:** `telegram-bot.service`
- **Proxy:** SOCKS5 on `127.0.0.1:2080`
