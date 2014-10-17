#![feature(slicing_syntax)]

extern crate redlock;
extern crate redis;
use redlock::RedLock;
use std::io::timer::sleep;
use std::time::duration::Duration;
use std::rand;
use std::rand::distributions::{IndependentSample, Range};
use std::os;

fn main() {


    let mut number_of_workers = 10u;
    let mut number_of_incrs   = 55u;
    let resource          = "incr_mutex";
    let incr_key          = "incr_key";

    let args = os::args();
    if args.len() == 3 {
        number_of_workers = from_str(args[1][]).unwrap();
        number_of_incrs = from_str(args[2][]).unwrap();
    }

    let result            = number_of_workers * number_of_incrs;

    let (tx, rx) = channel();

    println!("Starting {} workers, each incrementing {} times.", number_of_workers, number_of_incrs);

    let client = redis::Client::open("redis://127.0.0.1:6380/").unwrap();
    let con = client.get_connection().unwrap();
    redis::cmd("DEL").arg(incr_key).execute(&con);

    for _ in range(0u, number_of_workers) {
        let tx = tx.clone();
        spawn(proc() {
            let between = Range::new(0u, 5);
            let mut rng = rand::task_rng();

            let rl = RedLock::new(vec!["redis://127.0.0.1:6380/", "redis://127.0.0.1:6381/", "redis://127.0.0.1:6382/", ]);
            let con = rl.servers[0].get_connection().unwrap();

            for _ in range(0u, number_of_incrs) {
                let lock;
                loop {
                    match rl.lock(resource.as_bytes(), 1000) {
                        Some(l) => { lock = l; break }
                        None => ()
                    }
                }
                let val : int = redis::cmd("GET").arg(incr_key).query(&con).unwrap_or(0);

                let n = between.ind_sample(&mut rng);
                sleep(Duration::milliseconds(n as i64));

                redis::cmd("SET").arg(incr_key).arg(val+1).execute(&con);

                rl.unlock(&lock);
            }

            tx.send(());
        })
    }

    let mut i = 0;
    loop {
        if i == number_of_workers {
            break;
        }

        let _ = rx.recv();
        i += 1;
    }

    let actual_result : uint = redis::cmd("GET").arg(incr_key).query(&con).unwrap_or(0);

    println!("Expected result: {}", result);
    println!("Actual result:   {}", actual_result);
    if result == actual_result {
        println!("Everything fine! \\o/");
    } else {
        println!("Something is broken. /o\\");
    }
}
