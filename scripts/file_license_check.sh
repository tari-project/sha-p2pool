#!/usr/bin/env bash
#
# Must be run from the repo root
#

set -e

# rg -i "Copyright.*The Tari Project" --files-without-match \
#    -g '!*.{Dockerfile,asc,bat,config,config.js,css,csv,drawio,env,gitkeep,hbs,html,ini,iss,json,lock,md,min.js,ps1,py,rc,scss,sh,sql,svg,toml,txt,yml,vue}' . \
#    | sort > /tmp/rgtemp

# Exclude files without extensions as well as those with extensions that are not in the list
#
rgTemp=$(mktemp)
rg -i "Copyright.*The Tari Project" --files-without-match \
    -g '!*.{Dockerfile,asc,bat,config,config.js,css,csv,drawio,env,gitkeep,hbs,html,ini,iss,json,lock,md,min.js,ps1,py,rc,scss,sh,sql,svg,toml,txt,yml,vue}' . \
    | while IFS= read -r file; do
        if [[ -n $(basename "$file" | grep -E '\.') ]]; then
            echo "$file"
        fi
    done | sort > ${rgTemp}

# Sort the .license.ignore file as sorting seems to behave differently on different platforms
licenseIgnoreTemp=$(mktemp)
cat .license.ignore | sort > ${licenseIgnoreTemp}

DIFFS=$(diff -u --strip-trailing-cr ${licenseIgnoreTemp} ${rgTemp})

# clean up
rm -vf ${rgTemp}
rm -vf ${licenseIgnoreTemp}

if [ -n "$DIFFS" ]; then
    echo "New files detected that either need copyright/license identifiers added, or they need to be added to .license.ignore"
    echo "NB: The ignore file must be sorted alphabetically!"

    echo "Diff:"
    echo "$DIFFS"
    exit 1
fi
