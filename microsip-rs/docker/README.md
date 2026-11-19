# Building in Docker

Builds the Windows programs of MicroSIP-Rust in a Linux container: the same
checks and tests as `build-release.ps1`, then `microsip-gui.exe` and
`phone-engine.exe`. Nothing but Docker is needed on the computer: no Rust, no
Visual Studio.

## On Windows

1. Install Docker Desktop and start it (it runs Linux containers by default).
2. In this folder (`microsip-rs`) run `docker-build.cmd`.

The first run downloads the Rust image and the libraries and takes a while;
later runs reuse them (the volumes `microsip-rs-cargo` and
`microsip-rs-target`). The result is in `target\docker-out`: a folder and a
ZIP named `microsip-rs-windows-docker-<date>-<time>`, and the SHA256 of the
ZIP is printed at the end.

## Elsewhere

```sh
docker build -t microsip-rs-build docker
docker run --rm -v "$PWD":/src -v microsip-rs-cargo:/usr/local/cargo/registry \
  -v microsip-rs-target:/target -v "$PWD/target/docker-out":/out microsip-rs-build
```

## Notes

- The programs are linked with MinGW (`x86_64-pc-windows-gnu`) instead of
  MSVC. They need only the DLLs every Windows has.
- They are not signed; SmartScreen or Smart App Control may ask before the
  first start.
- The tests run on Linux inside the container. Sound devices are not used by
  them, so none are needed.
