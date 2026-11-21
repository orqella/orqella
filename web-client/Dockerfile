FROM nginxinc/nginx-unprivileged:1.27-alpine@sha256:65e3e85dbaed8ba248841d9d58a899b6197106c23cb0ff1a132b7bfe0547e4c0

RUN rm /etc/nginx/conf.d/default.conf
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY client /usr/share/nginx/html/client
COPY app-client /usr/share/nginx/html/app-client
COPY pkg /usr/share/nginx/html/pkg
COPY sw.js app-sw.js event-source-shim.js ws-shim.js app-view-shell-shim.js /usr/share/nginx/html/
COPY module-bootstrap.html module-bootstrap.js module-sw.js module-bridge.js /usr/share/nginx/html/
