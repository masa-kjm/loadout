#!/usr/bin/env bash
set -euo pipefail

scenario="${1:-all}"
root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT

home="$root/home"
config="$root/config"
state="$root/state"
mkdir -p "$home" "$state" "$config"
cp -R /opt/loadout-fixture/. "$config/"

export HOME="$home"
export XDG_CONFIG_HOME="$root/xdg-config"
export XDG_STATE_HOME="$state"
export LOADOUT_E2E_ROOT="$root"
config_path="$config/config.yaml"

run_loadout() {
    /usr/local/bin/loadout "$@"
}

assert_link() {
    local target="$1"
    local expected="$2"
    [[ -L "$target" ]] || { echo "expected symbolic link: $target" >&2; exit 1; }
    [[ "$(readlink "$target")" == "$expected" ]] || {
        echo "unexpected link target for $target" >&2
        exit 1
    }
}

run_smoke() {
    run_loadout validate --config "$config_path"
    run_loadout plan --config "$config_path"
    run_loadout apply --config "$config_path" --yes
    assert_link "$home/.gitconfig" "$config/stores/dotfiles/gitconfig"

    before="$(find "$home" "$state" "$config" -type f -o -type l | sort | xargs -r sha256sum)"
    run_loadout apply --config "$config_path" --yes
    after="$(find "$home" "$state" "$config" -type f -o -type l | sort | xargs -r sha256sum)"
    [[ "$before" == "$after" ]] || { echo "repeated apply changed the environment" >&2; exit 1; }

    run_loadout diff
}

case "$scenario" in
    shell)
        echo "Loadout manual test environment"
        echo "Config: $config_path"
        echo "Home:   $home"
        echo "State:  $state"
        echo "Try:    loadout validate --config $config_path"
        exec bash
        ;;
    all|smoke)
        run_smoke
        ;;
    validate)
        run_loadout validate --config "$config_path"
        ;;
    plan)
        run_loadout plan --config "$config_path"
        ;;
    apply)
        run_loadout apply --config "$config_path" --yes
        assert_link "$home/.gitconfig" "$config/stores/dotfiles/gitconfig"
        ;;
    diff)
        run_loadout diff
        ;;
    *)
        echo "unknown scenario: $scenario" >&2
        exit 2
        ;;
esac
