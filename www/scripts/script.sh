#!/bin/sh
count=0
while [ $count -lt 1000000 ];
do echo "hi $count"
count=$((count+1))
done;