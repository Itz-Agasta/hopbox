# AUR package

`hopbox-bin/` holds the `PKGBUILD` and `.SRCINFO` for the [`hopbox-bin`](https://aur.archlinux.org/packages/hopbox-bin) AUR package. It installs the prebuilt release binary for `x86_64` or `aarch64`. These files are the source of truth: the maintainer copies them to the AUR git repository on each release.

To install the package, run `paru -S hopbox-bin` or `yay -S hopbox-bin`. You don't need anything from this directory.

## Update for a new release

Maintainers only. After a release's archives are on GitHub:

1. Set `pkgver` to the new version and `pkgrel=1` in `hopbox-bin/PKGBUILD`.
2. Copy the two Linux checksums from the release's `sha256.sum` into `sha256sums_x86_64` and `sha256sums_aarch64`.
3. Run `makepkg --printsrcinfo > .SRCINFO` in `hopbox-bin/`.
4. Build once with `makepkg -si` to check it.
5. Clone `ssh://aur@aur.archlinux.org/hopbox-bin.git`, copy `PKGBUILD` and `.SRCINFO` into it, commit, and push with `git push origin main:master`.
