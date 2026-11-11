#!/bin/sh
# Compiles assets/randomizer/Launch3ds.java into the class the app carries,
# against FVX's jar: build-launcher.sh /path/to/UPR-FVX.jar
#
# FVX's own Java has no compiler, so this needs a JDK (21 or newer) on the
# PATH. The class is written for Java 21, older than the Java FVX carries.
set -eu
jar=${1:?usage: build-launcher.sh /path/to/UPR-FVX.jar}
here=$(cd "$(dirname "$0")/../../assets/randomizer" && pwd)
javac --release 21 -cp "$jar" -d "$here" "$here/Launch3ds.java"
echo "wrote $here/Launch3ds.class"
