# Provisioning sidecar for the Garage server, which ships as a static binary
# without a shell. The server itself runs the untouched dxflrs/garage image.
FROM dxflrs/garage:v2.5.0 AS garage

FROM alpine:3.22

COPY --from=garage /garage /garage
COPY bootstrap.sh /usr/local/bin/garage-bootstrap.sh
RUN chmod 0755 /usr/local/bin/garage-bootstrap.sh

ENTRYPOINT ["/bin/sh", "/usr/local/bin/garage-bootstrap.sh"]
