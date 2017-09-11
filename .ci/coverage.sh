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
make install DESTDIR=../tmp &&
cd ../.. &&
./kcov-master/tmp/usr/local/bin/kcov --exclude-pattern=/.cargo --coveralls-id=$TRAVIS_JOB_ID --verify  target/kcov target/debug/*-*[!.]? &&
echo "Uploaded coverage to coveralls!" &&
bash <(curl -s https://codecov.io/bash) -s target/kcov &&
echo "Uploaded code coverage to codecov!"
