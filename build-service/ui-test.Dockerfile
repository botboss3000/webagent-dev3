# Isolated frontend verification only; never an application runtime image.
# Upstream browser image and npm package versions must agree.
FROM mcr.microsoft.com/playwright@sha256:bc72a8df40831ded821e3cbc79c318c21b44d6e51dff9fd89ace7a8249c5b462
ENV PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
RUN npm install --prefix /opt/ui-tests --omit=optional --ignore-scripts --no-audit --no-fund playwright@1.64.0 \
 && printf '%s\n' "const p=require('playwright'); exports.chromium=p.chromium; exports.firefox=p.firefox; exports.webkit=p.webkit;" > /opt/ui-tests/playwright-entry.cjs
RUN node -e 'const p=require("/opt/ui-tests/node_modules/playwright"); require("fs").symlinkSync(p.chromium.executablePath(),"/opt/ui-tests/chromium")'
LABEL webagent.verification.recipe="ui-tests-v1-playwright-1.64.0"
