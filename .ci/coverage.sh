#!/bin/bash

# this is in a separate script from `.travis.yml` so we can set this option.
shopt -s extglob

wget https://github.com/SimonKagstrom/kcov/archive/master.tar.gz &&
tar xzf master.tar.gz &&
cd kcov-master &&
mkdir build &&
cd build &&
cmake .. &&
make &&
sudo make install &&
cd ../.. &&
rm -rf kcov-master

for file in target/debug/!(*.*); do
    [ -d "$file" ] && continue;
    mkdir -p "target/cov/$(basename $file)";
    kcov --exclude-pattern=/.cargo,/usr/lib --verify "target/cov/$(basename $file)" "$file";
done

bash <(curl -s https://codecov.io/bash) &&
echo "Uploaded code coverage"
