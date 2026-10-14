#!/usr/bin/env bash
# Bash preserves Cargo environment names containing hyphens; dash drops them.
# Preserve Cargo's compiler arguments and failures, with an optional shared cache.
if command -v sccache >/dev/null 2>&1 && sccache --dist-status >/dev/null 2>&1; then
    sccache "$@"
    cache_status=$?
    # sccache reports fatal server errors (such as a deleted TMPDIR) as -2/254.
    if [ "$cache_status" -ne 254 ]; then
        exit "$cache_status"
    fi
    echo "rustc-wrapper: sccache failed; retrying the compiler directly" >&2
fi
exec "$@"
