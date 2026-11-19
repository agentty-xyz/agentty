#!/usr/bin/env node
"use strict";
// Runs `{{ binary }}`, installing the binaries first when they are missing.

require("./binary.js").run({{ binary_json }});
