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

./kcov-master/tmp/usr/local/bin/kcov –coveralls-id=$TRAVIS_JOB_ID --verify –exclude-pattern=/.cargo target/kcov target/debug/!(*.*)  &&
echo "Uploaded code coverage"
