#!/bin/sh
# Image entrypoint: the emulator by default, the CSMS when the first argument names it.
#   docker run IMAGE --url ws://host:9000/ocpp --id CP-001     -> kirby-ocpp ...
#   docker run IMAGE kirby-csms --listen 0.0.0.0:9000          -> kirby-csms ...
set -eu
case "${1:-}" in
  kirby-ocpp | kirby-csms) exec "$@" ;;
  *) exec kirby-ocpp "$@" ;;
esac
