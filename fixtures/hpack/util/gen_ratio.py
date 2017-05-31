#!/usr/bin/env python
#
# This script takes directories which contain the hpack-test-case json
# files, and calculates the compression ratio in each file and outputs
# the result in table formatted in rst.
#
# The each directory contains the result of various HPACK compressor.
#
# The table is laid out so that we can see that how input header set
# in one json file is compressed in each compressor.
#
import sys, json, os, re, argparse

class Stat:
    def __init__(self, complen, srclen):
        self.complen = complen
        self.srclen = srclen

def compute_stat(jsdata):
    complen = 0
    srclen = 0
    for item in jsdata['cases']:
        complen += len(item['wire']) // 2
        srclen += \
                  sum([len(list(x.keys())[0]) + len(list(x.values())[0]) \
                       for x in item['headers']])
    return Stat(complen, srclen)

def format_result(r):
    return '{:.02f} ({}/{}) '.format(float(r.complen)/r.srclen,
                                     r.complen, r.srclen)

if __name__ == '__main__':
    ignores = [ '.git', 'raw-data', 'util' ]

    basedir = os.path.dirname(os.path.abspath(__file__)) + '/../'
    entries = [(os.path.basename(re.sub(r'/+$', '', p)), p) for p in os.listdir(basedir)]
    entries.sort()

    maxnamelen = 0
    maxstorynamelen = 0
    res = {}

    stories = set()
    for name, ent in entries:
        entdir = basedir + ent
        if (not os.path.isdir(entdir)) or (ent in ignores):
            continue

        files = [p for p in os.listdir(entdir) if p.endswith('.json')]
        res[name] = {}
        maxnamelen = max(maxnamelen, len(name))
        for fn in files:
            stories.add(fn)
            maxstorynamelen = max(maxstorynamelen, len(fn))
            with open(os.path.join(entdir, fn)) as f:
                input = f.read()
            rv = compute_stat(json.loads(input))
            res[name][fn] = rv
            maxnamelen = max(maxnamelen, len(format_result(rv)))
    stories = list(stories)
    stories.sort()

    overall = {}
    for name in res:
        r = Stat(0, 0)
        for _, stat in res[name].items():
            r.srclen += stat.srclen
            r.complen += stat.complen
        overall[name] = r
        maxnamelen = max(maxnamelen, len(format_result(r)))

    storynameformat = '|{{:{}}} |'.format(maxstorynamelen)
    nameformat = '{{:{}}} |'.format(maxnamelen)

    sys.stdout.write('''\
The each cell has X (Y/Z) format:  

**X**: Y / Z  
**Y**: Number of bytes after compression  
**Z**: Number of bytes before compression  

''')

    names = res.keys()
    names.sort()

    sys.stdout.write(storynameformat.format('story'))
    for name in names:
        sys.stdout.write(nameformat.format(name))
    sys.stdout.write('\n')

    sys.stdout.write('|')
    sys.stdout.write('-'*(maxstorynamelen+1))
    sys.stdout.write('|')
    for _ in names:
        sys.stdout.write('-'*(maxnamelen+1))
        sys.stdout.write('|')
    sys.stdout.write('\n')

    for story in stories:
        sys.stdout.write(storynameformat.format(story))
        srclen = -1
        for name in names:
            stats = res[name]
            if story not in stats:
                sys.stdout.write(nameformat.format('N/A'))
                continue
            if srclen == -1:
                srclen = stats[story].srclen
            elif srclen != stats[story].srclen:
                raise Exception('Bad srclen')
            sys.stdout.write(nameformat.format(format_result(stats[story])))
        sys.stdout.write('\n')

    sys.stdout.write(storynameformat.format('Overall'))
    for name in names:
        sys.stdout.write(nameformat.format(format_result(overall[name])))
    sys.stdout.write('\n')

