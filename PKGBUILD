pkgname=ianny-custom
pkgver=3.0.0
pkgrel=1
pkgdesc="Desktop utility that helps preventing repetitive strain injuries by periodically informing the user to take breaks."
arch=('x86_64')
url="https://github.com/mgabor3141/ianny"
license=('GPL-3.0-only')
depends=(dbus glibc gcc-libs)
makedepends=(cargo meson jj)
provides=(ianny)
conflicts=(ianny)

pkgver() {
	cd "$startdir"
	printf "3.0.0.r%s.%s" "$(jj log -r 'ancestors(@-, 100)' --no-graph -T 'commit_id' 2>/dev/null | wc -l)" "$(jj log -r '@-' --no-graph -T 'commit_id.short()' 2>/dev/null)"
}

prepare() {
	rsync -a --exclude='.jj' --exclude='target' --exclude='pkg' --exclude='src' \
		"$startdir/" "$srcdir/$pkgname/"
	cd "$srcdir/$pkgname"
	export RUSTUP_TOOLCHAIN=stable
	arch-meson build
}

build() {
	cd "$srcdir/$pkgname"
	export RUSTUP_TOOLCHAIN=stable
	export CARGO_TARGET_DIR=target
	meson compile -C build
}

package() {
	cd "$srcdir/$pkgname"
	meson install -C build --destdir "${pkgdir}"
}
