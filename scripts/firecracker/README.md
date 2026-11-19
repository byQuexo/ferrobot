# Firecracker guest

`ferrobotd --backend firecracker` runs each agent in its own Firecracker
microVM. Docker stays the default; this backend needs Linux with KVM.

**Nothing here has booted a microVM yet** (written on macOS, no `/dev/kvm`).
Untested parts are marked `TODO(kvm)` in the code and scripts.

## Host requirements

- Linux on x86_64 or aarch64 with read/write access to `/dev/kvm`.
- The `firecracker` binary ([releases](https://github.com/firecracker-microvm/firecracker/releases)).
- `iproute2` and root (or `CAP_NET_ADMIN`): the daemon creates one TAP device
  per VM (`fctap<N>`) and attaches it to the bridge.
- A bridge set up once. Guests get `172.16.0.2`, `.3`, ... and reach the
  daemon at the bridge IP:

  ```sh
  ip link add fcbr0 type bridge
  ip addr add 172.16.0.1/24 dev fcbr0
  ip link set fcbr0 up
  ```

  For internet access add NAT for `172.16.0.0/24`, as in Firecracker's
  [network-setup.md](https://github.com/firecracker-microvm/firecracker/blob/main/docs/network-setup.md).
- Guests are isolated from each other on the bridge, not from the host. Allow
  them only the agent API port:

  ```sh
  iptables -A INPUT -i fcbr0 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
  iptables -A INPUT -i fcbr0 -p tcp --dport 7879 -j ACCEPT
  iptables -A INPUT -i fcbr0 -j DROP
  ```

## Kernel

Use an uncompressed guest kernel from Firecracker's CI artifacts, as shown in
[getting-started.md](https://github.com/firecracker-microvm/firecracker/blob/main/docs/getting-started.md#getting-a-rootfs-and-guest-kernel-image)
(`vmlinux-<version>` under `spec.ccfc.min`). The guest network is configured
with the kernel's `ip=` argument, so the kernel needs `CONFIG_IP_PNP`.

## Root filesystem

```sh
docker build -f runner/Dockerfile -t ferrobot-runner:dev .   # optional, the script builds it if missing
sudo scripts/firecracker/build-rootfs.sh rootfs.ext4 2G
```

The script exports the runner image, adds `iproute2` and
`/sbin/ferrobot-init`, and packs it with `mkfs.ext4 -d`. All VMs share this
image read-only. Each VM also gets a sparse home disk (`/home/node`), which
the init formats on first boot.

## Run

```sh
ferrobotd --backend firecracker \
  --fc-kernel vmlinux --fc-rootfs rootfs.ext4 \
  --agent-listen 172.16.0.1:7879
```

`--fc-host-ip` (default `172.16.0.1`) and `--fc-bridge` (default `fcbr0`)
must match the bridge. Runners get `FERROBOT_DAEMON_URL=http://<host ip>:<agent port>`.
VM state lives in `<data dir>/vms/<agent>/`.

## Not done yet

- Nothing was run on KVM: boot, TAP setup, MMDS, home disk, graceful stop.
- No jailer (roadmap F3); firecracker runs as the daemon's user.
- `exec`, file access and snapshots need the guest agent (F2) and snapshot
  sleep (F4b); they return "not implemented". Cold sleep is a plain stop.
- No DNS in the guest (`/etc/resolv.conf` is empty).
- Stopping the daemon kills its VMs without a guest shutdown.
