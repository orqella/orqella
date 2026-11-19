FROM nginx:1.27-alpine

RUN rm /etc/nginx/conf.d/default.conf
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY client /usr/share/nginx/html/client
COPY app-client /usr/share/nginx/html/app-client
COPY pkg /usr/share/nginx/html/pkg
COPY sw.js app-sw.js event-source-shim.js ws-shim.js app-view-shell-shim.js /usr/share/nginx/html/
