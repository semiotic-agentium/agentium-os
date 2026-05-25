import http from "k6/http";
import { check, sleep } from "k6";

const targetUrl = __ENV.TARGET_URL || "http://checkout-api:8080";
const checkoutPath = __ENV.CHECKOUT_PATH || "/api/checkout";
const ordersPath = __ENV.ORDERS_PATH || "/api/orders";

export const options = {
  scenarios: {
    baseline_checkout_traffic: {
      executor: "constant-arrival-rate",
      rate: Number(__ENV.RPS || "50"),
      timeUnit: "1s",
      duration: __ENV.DURATION || "24h",
      preAllocatedVUs: Number(__ENV.PREALLOCATED_VUS || "80"),
      maxVUs: Number(__ENV.MAX_VUS || "200"),
    },
  },
  thresholds: {
    http_req_failed: ["rate<0.20"],
  },
};

export default function () {
  const path = Math.random() < 0.85 ? checkoutPath : ordersPath;
  const response = http.get(`${targetUrl}${path}`, {
    tags: {
      service: "checkout-api",
      route: path,
      workload: "baseline",
    },
    timeout: "10s",
  });

  check(response, {
    "checkout reachable": (r) => r.status >= 200 && r.status < 500,
  });

  sleep(0.01);
}
