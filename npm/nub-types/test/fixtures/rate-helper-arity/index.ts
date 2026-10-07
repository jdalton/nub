import { debounce, throttle } from "node:util";

debounce(function (value: string) {
  this(123);
  return value;
}, 1);

throttle(function (value: number) {
  this();
  return value;
}, 1, 1);
