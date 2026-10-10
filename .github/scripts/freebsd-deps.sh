# Sourced (". .github/scripts/freebsd-deps.sh") inside the FreeBSD 14 VM by CI
# and release, under `set -eux`. Installs the build dependencies and makes sure
# pkg-config can find everything gdk-sys/webkit2gtk need. Exports
# PKG_CONFIG_PATH when a shim directory is required.

sudo pkg install -y rust pkgconf bzip2
sudo pkg install -y webkit2-gtk_41

# freetype2.pc (pulled in by gdk-3.0.pc) requires 'bzip2' (and zlib), but the
# FreeBSD base system ships libbz2/libz without .pc files unless the port
# package provides one. Create minimal shims when pkg-config cannot find them.
mdv_pc_dir="$HOME/mdv-pkgconfig"
mkdir -p "$mdv_pc_dir"
mdv_shim() { # name version libs
  if ! pkg-config --exists "$1"; then
    cat >"$mdv_pc_dir/$1.pc" <<PC
prefix=/usr
libdir=/usr/lib
includedir=/usr/include
Name: $1
Description: $1 (FreeBSD base system shim)
Version: $2
Libs: -L\${libdir} $3
Cflags: -I\${includedir}
PC
    PKG_CONFIG_PATH="$mdv_pc_dir${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
    export PKG_CONFIG_PATH
  fi
}
mdv_shim bzip2 1.0.8 -lbz2
mdv_shim zlib 1.3 -lz

# Fail early with a readable diagnostic instead of deep inside a build script.
for m in gdk-3.0 gtk+-3.0 webkit2gtk-4.1 libsoup-3.0; do
  if ! pkg-config --print-errors --exists "$m"; then
    echo "pkg-config cannot find $m; available modules:" >&2
    pkg-config --list-all >&2 || true
    exit 1
  fi
done
