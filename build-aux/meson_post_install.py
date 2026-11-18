#!/usr/bin/env python3

import sys
import os
from subprocess import call

datadir = sys.argv[1]
bindir = sys.argv[2]
app_name = sys.argv[3]

print(f"""
### executing post-install script with arguments: ###
    datadir: {datadir}
    bindir: {bindir}
    app_name: {app_name}
""", file=sys.stderr)

if not os.environ.get("DESTDIR", ""):
    print("Rebuilding font cache...", file=sys.stderr)
    call(["fc-cache", "-v", "-f"])

print("### post-install script finished ###", file=sys.stderr)
