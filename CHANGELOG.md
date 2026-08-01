# Changelog

## [0.1.35](https://github.com/celox-sim/celox/compare/v0.1.34...v0.1.35) (2026-08-01)


### Features

* **bench:** add a dedicated Heliodor benchmark harness ([aed35e6](https://github.com/celox-sim/celox/commit/aed35e603d1383a0184f1fb11362f07d05898fc9))


### Bug Fixes

* align sliced dependencies with lowering ([8c96d3a](https://github.com/celox-sim/celox/commit/8c96d3a61f4623db098bbeb37cbf4f9451da2b6d))
* **ci:** disable benchmark debuginfo ([7773da3](https://github.com/celox-sim/celox/commit/7773da3c80776d7af4ba90b5f09ce23c05b3bb65))
* **ci:** disable benchmark debuginfo ([5cd80b7](https://github.com/celox-sim/celox/commit/5cd80b7182a39cb3561d0a377d481b4eee619d24))
* **ci:** extend Heliodor gate timeout ([5e35668](https://github.com/celox-sim/celox/commit/5e35668f2065f64dd19e06f131ddcab1c229e016))
* **ci:** extend Heliodor gate timeout ([66026a3](https://github.com/celox-sim/celox/commit/66026a309fb323fc71ca12dee0d7ed779dfb3ca5))
* **ci:** install ripgrep for Heliodor ([349df8b](https://github.com/celox-sim/celox/commit/349df8b18533abb357b16bb21183d8e1dfd170bd))
* **ci:** let benchmark runs finish ([13b5848](https://github.com/celox-sim/celox/commit/13b5848854f69218fe6f10a142a4f48a67623257))
* **ci:** let benchmark runs finish ([62ea240](https://github.com/celox-sim/celox/commit/62ea2402ac9a1c8131a80e91b9859303eecf83ff))
* **ci:** mint release tokens from GitHub App ([f6102bd](https://github.com/celox-sim/celox/commit/f6102bda9d1e393cd8fdfa47f7593f17bc909c0b))
* **ci:** mint release tokens from GitHub App ([f605361](https://github.com/celox-sim/celox/commit/f605361ae21ecef4baff941aef6f69d0c2d3f05e))
* **ci:** normalize Heliodor optimization level ([dab15b6](https://github.com/celox-sim/celox/commit/dab15b6faf3a875c0d9afedcb1cfaf42cd2358d7))
* **ci:** normalize Heliodor optimization level ([359a52f](https://github.com/celox-sim/celox/commit/359a52f154074d36d86cea23445d0ff2bed9036e))
* **ci:** remove Heliodor ripgrep dependency ([fbe455f](https://github.com/celox-sim/celox/commit/fbe455f73eb270941eebe9e22e5f56c3c62472e0))
* **ci:** remove Heliodor ripgrep dependency ([5f89500](https://github.com/celox-sim/celox/commit/5f895001630c2491d0b341248b2b3b56e7c7e82b))
* **ci:** skip product tests for release PRs ([b4fac74](https://github.com/celox-sim/celox/commit/b4fac74c31c0c271248f8e4fbd799495d48e51e6))
* **ci:** skip product tests for release PRs ([29cb509](https://github.com/celox-sim/celox/commit/29cb509106e1d56c10c3d5a1054d4ebafb9747e4))
* **ci:** validate PR titles from current default branch ([f9ddd47](https://github.com/celox-sim/celox/commit/f9ddd47849ec856fa12a23bc4d422eaafc4001a7))
* **ci:** validate PR titles from current default branch ([bbda83c](https://github.com/celox-sim/celox/commit/bbda83c48ec78316b7ba84cd86adea0eeede88ec))
* **cli:** accept case-insensitive optimization levels ([89248e9](https://github.com/celox-sim/celox/commit/89248e9b482841661fe5d6f356ffe2c9dc6760ab))
* **cli:** accept case-insensitive optimization levels ([356fddd](https://github.com/celox-sim/celox/commit/356fddda465a654fb57d618705cb925f8669ca7b))
* **cranelift:** adapt to 0.134 API ([6648f80](https://github.com/celox-sim/celox/commit/6648f80aa2be4450dc6fccb8d69e8596213f405a))
* **deps:** refresh Cargo.lock for master base ([b615cd9](https://github.com/celox-sim/celox/commit/b615cd9566d5b7b56bee970b3ca537f32a746bf3))
* **deps:** update cranelift crates to 0.134.0 ([cdc8904](https://github.com/celox-sim/celox/commit/cdc8904b5552ce883e1064d4c91242bdaed2a9f1))
* **deps:** update cranelift crates to 0.134.0 ([e0f95b2](https://github.com/celox-sim/celox/commit/e0f95b2012c6bad27559a9bf683e1a70a1cbfc54))
* honor Cranelift codegen trace options ([c64d03a](https://github.com/celox-sim/celox/commit/c64d03a01ab6bba0f0e9e3b191a07a8ffc975788))
* **hooks:** allow uninitialized submodules ([7432ddd](https://github.com/celox-sim/celox/commit/7432ddd6558cc49c7028fe1e15ff1308dc1e7d9d))
* **hooks:** allow uninitialized submodules ([6623366](https://github.com/celox-sim/celox/commit/6623366e950ffd31d9537cbfcf5f5b632c0342d3))
* preserve dependencies in guarded SIR regions ([f05487d](https://github.com/celox-sim/celox/commit/f05487d3824010a9706bb1103ad874ff38ad7b5b))
* preserve logical widths in dynamic state access ([c687e5d](https://github.com/celox-sim/celox/commit/c687e5d668699ec9f575fec5c496691c7c5868d8))
* preserve propagated RTL atom boundaries ([9e01add](https://github.com/celox-sim/celox/commit/9e01add76bba8e1a4cd728b330291ef9c3266875))
* **regalloc:** preserve edge reconstruction pressure ([bef13f1](https://github.com/celox-sim/celox/commit/bef13f1ebae106f7791fd42582f9a1ce4320cdb7))
* **regalloc:** preserve edge reconstruction pressure ([b249f52](https://github.com/celox-sim/celox/commit/b249f522403980c3641c40e0df08d70a1d58b59c))
* remove unused host dependencies from wasm build ([db4d6b2](https://github.com/celox-sim/celox/commit/db4d6b2334b69b4d0bfc551c4fe4570e5e8da52c))
* **renovate:** group emnapi package updates ([dd92c72](https://github.com/celox-sim/celox/commit/dd92c723df07d025da3a7a6f459aba8f0abcaf0c))
* **renovate:** group emnapi package updates ([a59a738](https://github.com/celox-sim/celox/commit/a59a7387990a6bb7f7b9a2c258eaaf5af948c5fe))
* scope native runtime extraction to x86 ([39677aa](https://github.com/celox-sim/celox/commit/39677aa24d260f37f25452af26a50c77ef4f87b5))


### Performance Improvements

* coalesce pointwise RTL atoms by source identity ([76f10f2](https://github.com/celox-sim/celox/commit/76f10f253db92f7abdec7294bdd3b9ea18c82bae))
* reuse block-local dynamic state loads ([3014472](https://github.com/celox-sim/celox/commit/301447216c234e5fd94c4c47f3c9d574576319ac))


### Reverts

* drop redundant Heliodor setup ([625511f](https://github.com/celox-sim/celox/commit/625511f8b540d04ff7a426bb57a19613c9dc75cf))

## Changelog

Celox releases are generated from Conventional Commit pull request titles.
