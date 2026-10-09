# Shared setup for the build scripts: finds the toolchains in their usual places
# on macOS and Linux. Override any of them by setting the variable beforehand.
#
#   ANDROID_HOME       Android SDK          (default: ~/Library/Android/sdk or ~/Android/Sdk)
#   ANDROID_NDK_HOME   Android NDK          (default: newest under $ANDROID_HOME/ndk)
#   JAVA_HOME          JDK 17+ for Gradle   (default: Android Studio's bundled JDK, if found)
#   LLVM_BIN           folder with llvm-rc, lld-link, llvm-lib (Windows cross-builds; default: PATH)

[ -d "$HOME/.cargo/bin" ] && PATH="$HOME/.cargo/bin:$PATH"
[ -n "${LLVM_BIN:-}" ] && PATH="$LLVM_BIN:$PATH"
export PATH

if [ -z "${ANDROID_HOME:-}" ]; then
    for dir in "$HOME/Library/Android/sdk" "$HOME/Android/Sdk"; do
        [ -d "$dir" ] && ANDROID_HOME="$dir" && break
    done
fi
export ANDROID_HOME="${ANDROID_HOME:-}"

if [ -z "${ANDROID_NDK_HOME:-}" ] && [ -d "$ANDROID_HOME/ndk" ]; then
    ANDROID_NDK_HOME=$(ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort -V | tail -1)
fi
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-}"

if [ -z "${JAVA_HOME:-}" ]; then
    for dir in "/Applications/Android Studio.app/Contents/jbr/Contents/Home" \
               "$HOME/android-studio/jbr" "/opt/android-studio/jbr" "/usr/local/android-studio/jbr"; do
        [ -d "$dir" ] && JAVA_HOME="$dir" && break
    done
fi
[ -n "${JAVA_HOME:-}" ] && export JAVA_HOME

ADB="${ANDROID_HOME:+$ANDROID_HOME/platform-tools/}adb"
