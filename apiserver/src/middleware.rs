// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use std::time::Instant;

use actix_web::{HttpRequest, HttpResponse};
use actix_web::error::Result;
use actix_web::middleware::{Finished, Middleware, Started};
use slog::Logger;
use time_ext::DurationExt;

pub struct SLogger {
    logger: Logger,
}

impl SLogger {
    pub fn new(logger: Logger) -> SLogger {
        SLogger { logger: logger }
    }

    fn time_cost<S>(&self, req: &mut HttpRequest<S>) -> Option<String> {
        req.extensions().get::<Instant>().map(|start| {
            let delta = start.elapsed().as_micros_unchecked();

            format!("{:.3}\u{00B5}s", delta)
        })
    }
}

impl<S> Middleware<S> for SLogger {
    fn start(&self, req: &mut HttpRequest<S>) -> Result<Started> {
        req.extensions_mut().insert(Instant::now());

        Ok(Started::Done)
    }

    fn finish(&self, req: &mut HttpRequest<S>, resp: &HttpResponse) -> Finished {
        let cost = self.time_cost(req).unwrap_or("".to_string());

        info!(
            self.logger,
            "{} {} {} {}",
            resp.status().as_u16(),
            req.method(),
            req.path(),
            cost
        );

        Finished::Done
    }
}
