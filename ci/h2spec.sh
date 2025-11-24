#!/bin/bash
LOGFILE="/tmp/h2server.log"

override_h2spec=false

# Check for optional flag
while getopts "F" opt; do
    case $opt in
        F) override_h2spec=true ;;
        *) echo "Usage: $0 [-o]"; exit 1 ;;
    esac
done

if ! [ -e "/tmp/h2spec" ] || $override_h2spec ; then
    # if we don't already have a h2spec executable, wget it from github
    if [[ "$OSTYPE" == "darwin"* ]]; then
        curl -L -o /tmp/h2spec_darwin_amd64.tar.gz https://github.com/summerwind/h2spec/releases/download/v2.1.1/h2spec_darwin_amd64.tar.gz \
            && tar xf /tmp/h2spec_darwin_amd64.tar.gz -C /tmp
    else
        curl -L -o /tmp/h2spec_linux_amd64.tar.gz https://github.com/summerwind/h2spec/releases/download/v2.1.1/h2spec_linux_amd64.tar.gz \
            && tar xf /tmp/h2spec_linux_amd64.tar.gz -C /tmp
    fi
fi

cargo build --example server
exec 3< <(./target/debug/examples/server);
SERVER_PID=$!

# wait 'til the server is listening before running h2spec, and pipe server's
# stdout to a log file.
sed '/listening on Ok(127.0.0.1:5928)/q' <&3 ; cat <&3 > "${LOGFILE}" &

# run h2spec against the server, printing the server log if h2spec failed
/tmp/h2spec -p 5928
H2SPEC_STATUS=$?
if [ "${H2SPEC_STATUS}" -eq 0 ]; then
    echo "h2spec passed!"
else
    echo "h2spec failed! server logs:"
    cat "${LOGFILE}"
fi
kill "${SERVER_PID}"
exit "${H2SPEC_STATUS}"
