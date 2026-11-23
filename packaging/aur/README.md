# AUR package

`hopbox-bin/` holds the `PKGBUILD` and `.SRCINFO` for the [`hopbox-bin`](https://aur.archlinux.org/packages/hopbox-bin) AUR package. It installs the prebuilt release binary for `x86_64` or `aarch64`.

## Publish the first version

The release archives must be downloadable, so the GitHub repo has to be public first.

```sh
git clone ssh://aur@aur.archlinux.org/hopbox-bin.git /tmp/hopbox-bin
cp packaging/aur/hopbox-bin/{PKGBUILD,.SRCINFO} /tmp/hopbox-bin/
cd /tmp/hopbox-bin
makepkg -si        # build and install it once to check it
git add PKGBUILD .SRCINFO
git commit -m "hopbox-bin 1.0.0"
git push
```

You need an AUR account with your SSH key added. Users then install it with `paru -S hopbox-bin` or `yay -S hopbox-bin`.

## Update for a new release

1. Set `pkgver` to the new version and `pkgrel=1` in `PKGBUILD`.
2. Copy the two Linux checksums from the release's `sha256.sum` into `sha256sums_x86_64` and `sha256sums_aarch64`.
3. Run `makepkg --printsrcinfo > .SRCINFO`, then copy both files to the AUR clone, commit, and push.
