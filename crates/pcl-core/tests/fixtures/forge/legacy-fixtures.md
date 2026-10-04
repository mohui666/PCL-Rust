# Legacy Forge fixtures

The `legacy-*.synthetic.*` and `middle-*.synthetic.json` files are **synthetic inputs**, not downloaded Forge release metadata. Tests generate inert ZIP/JAR payloads locally and never execute their contents or Java.

The schema and branching follow the fixed PCL 2.13.1.1 source at commit `0e0d12fdce6a2804916fb2be60e41144da637c18`:

- `Plain Craft Launcher 2/Pages/PageDownload/ModDownloadLib.vb:1330–1339`: the legacy `install` / `versionInfo` branch copies only `install.filePath` to the Maven location `install.path`, rewrites the profile ID and supplies `inheritsFrom` when missing.
- `Plain Craft Launcher 2/Pages/PageDownload/ModDownloadLib.vb:1316–1328`: the middle branch reads the embedded JSON named by `json` and copies the `maven/` tree to libraries. Rust extracts only the client profile's declared Maven files, after metadata/path/digest verification, instead of copying unrelated files.
- `Plain Craft Launcher 2/Modules/Minecraft/ModDownload.vb:630–634`: filename branch fixes for Forge 11.15.1.2318, 11.15.1.1902, 11.15.1.1890 and Minecraft 1.7.10 build 1300 onward.
- `Plain Craft Launcher 2/Modules/Minecraft/ModDownload.vb:672–730`: the official HTML table selects `installer`, `universal`, or `client` and provides installer MD5. Rust installs only `installer`.

The tests cover selected version identity, installer MD5, embedded JAR structure, external library SHA1, preserved existing files, cancellation, parent inheritance, and generated Windows/macOS launch arguments. They do not establish current official endpoint availability, compatibility with a real historical installer, execution of Forge, or Windows runtime behavior.

The middle branch also follows the official [Installer JSON dispatch](https://github.com/MinecraftForge/Installer/blob/2.0/src/main/java/net/minecraftforge/installer/json/Util.java) for supported spec values (missing/0/1), and [Install metadata](https://github.com/MinecraftForge/Installer/blob/2.0/src/main/java/net/minecraftforge/installer/json/Install.java) for processor sides. It is restricted to the historical Forge route and refuses any client processor. The modern spec-1 processor validator is unchanged. Official source was read; no installer JAR was fetched for this increment.

Middle tests include two declared embedded libraries (one with independent installer metadata SHA1/size, one without), one external SHA1-checked library, unreferenced Maven files that must not be installed, unsafe archive entries, wrong metadata, cancellation, user-file conflicts, cache reuse, and both platforms' launch argument construction. An exact duplicate ZIP fixture demonstrates `zip` 2.x's filename-map deduplication; historical opening compares the original ZIP central record count with the visible entries to reject this ambiguity.

Additional legacy native libraries, historical `map_to_resources` parents, mixed modern/legacy game arguments, client processors in the middle route, and ZIP64/multipart/self-extracting historical installers remain explicitly unsupported. Installer SHA1 is locally computed after the official legacy MD5 check. Per-library `_pcl_checksum_source` distinguishes independently supplied installer SHA1 from SHA1 derived from an embedded file without its own checksum. No claim of all historical Forge releases or real historical game execution is made.
