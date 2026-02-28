#!/bin/sh
count=0
while [ $count -lt 100000 ];
do echo "$count"
count=$((count+1))
done;