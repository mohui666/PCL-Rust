# Bundled launch compatibility patches

JAR files are copied byte-for-byte from the public PCL repository at commit `0e0d12fdce6a2804916fb2be60e41144da637c18`, `PCLCS/Launch/`. They are not downloaded from pack metadata.

- JavaWrapper.jar: [Java Launch Wrapper](https://github.com/00ll00/java_launch_wrapper), MIT; license in `JavaWrapper-LICENCE`. SHA256 `88eb3ee58854e916437831697ef6a2fd493da9bc4ffb018f620cd88f090952aa`. Windows-only encoding workaround; bundled bytecode requires Java 6+.
- LwjglUnsafeAgent.jar: [LWJGL Unsafe Agent](https://github.com/HMCL-dev/lwjgl-unsafe-agent), Apache-2.0; license in `LwjglUnsafeAgent-LICENSE`. SHA256 `6c07d3508dc090a12076876bdc227a19ce195a41c9e2c6535c3f82213b1b2015`. Bundled bytecode requires Java 25+; selected only for LWJGL 3.4.1.

License texts were retrieved from the respective public upstream source on 2026-10-05. No JAR is executed while constructing a launch plan or running isolated fixtures.
