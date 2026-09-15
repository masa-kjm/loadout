#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/../../../.." && pwd)"
image="loadout-v020-e2e"
scenario="${1:-all}"

case "$scenario" in
    all|smoke|validate|plan|apply|diff|shell) ;;
    *)
        echo "usage: $0 [all|smoke|validate|plan|apply|diff]" >&2
        exit 2
        ;;
esac

docker build --tag "$image" --file "$script_dir/Dockerfile" "$repo_root"
if [[ "$scenario" == "shell" ]]; then
    docker run --rm -it "$image" shell
else
    docker run --rm "$image" "$scenario"
fi
