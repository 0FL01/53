-keep class uniffi.dmsg_core.** { *; }
-keep class com.sun.jna.** { *; }
-dontwarn com.sun.jna.**
# UniFFI scaffolding uses reflection-free JNA direct mapping; keep entry points.
-keepclasseswithmembernames class * {
    native <methods>;
}
