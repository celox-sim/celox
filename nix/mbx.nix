{ pkgs }:
let
  releases = {
    x86_64-linux = {
      arch = "x86_64";
      hash = "sha256-4EjlrllX/hhQbh2v0Unogf43PJ4cDTrXqUpUFJQgzLs=";
    };
    aarch64-linux = {
      arch = "aarch64";
      hash = "sha256-vbFa4MxqEk53ClPsUqwI9toMxXTv5Nx/eHiE6tjaNfU=";
    };
  };

in
pkgs.stdenvNoCC.mkDerivation (finalAttrs: {
  pname = "mbx";
  version = "1.14.0";

  src = pkgs.fetchurl {
    url = "https://github.com/jdx/mr-boxington/releases/download/v${finalAttrs.version}/mbx-${
      releases.${pkgs.stdenv.hostPlatform.system}.arch
    }-unknown-linux-musl.tar.gz";
    hash = releases.${pkgs.stdenv.hostPlatform.system}.hash;
  };

  sourceRoot = ".";
  dontConfigure = true;
  dontBuild = true;

  installPhase = ''
    runHook preInstall
    install -Dm755 mbx $out/bin/mbx
    runHook postInstall
  '';

  meta = {
    description = "A shared cache for Cargo builds";
    homepage = "https://mr-boxington.jdx.dev";
    license = pkgs.lib.licenses.mit;
    mainProgram = "mbx";
    platforms = [
      "x86_64-linux"
      "aarch64-linux"
    ];
    sourceProvenance = [ pkgs.lib.sourceTypes.binaryNativeCode ];
  };
})
