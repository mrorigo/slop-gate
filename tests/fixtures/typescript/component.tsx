import React from "react";

export function Greeting(props: { name: string }) {
  return <span>Hello, {props.name}</span>;
}
