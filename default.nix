# Non-flake entrypoint that still uses flake-locked inputs for reproducibility.
let
  flake = builtins.getFlake (toString ./.);
in
  flake.packages.${builtins.currentSystem}.default
