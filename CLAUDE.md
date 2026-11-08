## Material Design 3 / M3 Expressive

The M3 spec lives at `.claude/skills/m3-expressive/`.

Before writing or reviewing any Material Design UI (components, dp values, corner radii,
motion springs, type styles, color roles, adaptive layouts), read
`.claude/skills/m3-expressive/SKILL.md` and follow its "Where to look" table into the
specific reference file.

Never state a number, token name, or component name from memory. Look it up:
- exact values (type scale, radii, springs, elevation) -> `references/tokens.md`
- per-component dp geometry -> `references/component-tokens.md`
- component behavior, variants, do/don'ts -> `references/components/*.md`
- platform API availability -> `references/platforms.md`

If a value genuinely isn't in M3, say so and label it "my design decision (not in M3)"
rather than presenting it as spec.

## Android app (`osmo-android/`)

Kotlin + Jetpack Compose, M3 Expressive theme, Navigation 3. The Rust SDK comes in through `:sdk`
(UniFFI bindings + `.so` from `scripts/build-sdk-android.sh`). Android skills live in
`~/.claude/plugins/marketplaces/android-skills` (edge-to-edge, navigation-3, adaptive, testing-setup).
Build, test and phone install: `docs/android-dev.md`. Run Gradle with
`JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"`.
