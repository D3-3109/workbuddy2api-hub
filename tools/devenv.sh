# Source this from Git Bash on Windows to cross-build Linux musl binaries:
#   source tools/devenv.sh && cargo zigbuild --release --target x86_64-unknown-linux-musl
# Needs: rustup (gnu host) + musl target, scoop mingw (dlltool), `pip install ziglang`,
# cargo-zigbuild, scoop perl's pure-Perl modules in ~/.perl5lib, ~/bin/zig-ranlib shim.
# On Linux none of this is needed: plain `cargo build --release` works.
_zigdir="$(python -c 'import ziglang,os;print(os.path.dirname(ziglang.__file__))')"
export PATH="$HOME/scoop/apps/mingw/current/bin:$HOME/.cargo/bin:$_zigdir:$PATH"
export MSYS2_ENV_CONV_EXCL="PERL5LIB"
export PERL5LIB="$(cygpath -u "$HOME/.perl5lib")"
export LC_ALL=C
# OpenSSL's generated object paths exceed legacy Windows path limits under
# deeply nested workspaces. Keep build products outside the source tree.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.zcode/rust-build}"
export RANLIB_x86_64_unknown_linux_musl="$HOME/bin/zig-ranlib"
unset _zigdir
