# Mergify CLI for stacked pull requests. Upstream publishes proprietary prebuilt
# binaries only; update the version and both hashes from the release's SHA256SUMS.
{
  lib,
  stdenv,
  fetchurl,
  autoPatchelfHook,
}:
let
  version = "2026.10.5";
  targets = {
    x86_64-linux = {
      triple = "x86_64-unknown-linux-gnu";
      hash = "sha256-Cayx2QYGFpKk1irJ0tWfT4PH18ctJElgfxSbzp8eoS4=";
    };
    aarch64-linux = {
      triple = "aarch64-unknown-linux-gnu";
      hash = "sha256-6vY8YtV4LCHlUk4u6GLTSJWkNUigvoOMcWsD7YLoG5k=";
    };
  };
  target =
    targets.${stdenv.hostPlatform.system}
      or (throw "mergify-cli: unsupported system ${stdenv.hostPlatform.system}");
in
stdenv.mkDerivation {
  pname = "mergify-cli";
  inherit version;

  src = fetchurl {
    url = "https://github.com/Mergifyio/mergify-cli/releases/download/${version}/mergify-${version}-${target.triple}.tar.gz";
    inherit (target) hash;
  };
  sourceRoot = ".";

  nativeBuildInputs = [ autoPatchelfHook ];
  buildInputs = [ stdenv.cc.cc.lib ];

  installPhase = ''
    runHook preInstall
    install -Dm755 mergify $out/bin/mergify
    install -Dm644 -t $out/share/licenses/mergify-cli LICENSE THIRD_PARTY_LICENSES
    runHook postInstall
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    $out/bin/mergify --version
  '';

  meta = {
    description = "Mergify CLI (stacked pull requests)";
    homepage = "https://github.com/Mergifyio/mergify-cli";
    license = lib.licenses.unfree;
    mainProgram = "mergify";
    platforms = builtins.attrNames targets;
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
}
