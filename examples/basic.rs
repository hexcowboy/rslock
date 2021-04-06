use rand::Rng;
use redlock::RedLock;

use std::env;
use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;

pub fn main() {
    let mut number_of_workers = 10;
    let mut number_of_incrs = 55;
    let resource = "incr_mutex";
    let incr_key = "incr_key";

    let mut args = env::args();
    if args.len() == 3 {
        number_of_workers = args.next().unwrap().parse().unwrap();
        number_of_incrs = args.next().unwrap().parse().unwrap();
    }

    let result: u32 = number_of_workers * number_of_incrs;

    let (tx, rx) = channel();

    println!(
        "Starting {} workers, each incrementing {} times.",
        number_of_workers, number_of_incrs
    );

    let client = redis::Client::open("redis://127.0.0.1:6380/").unwrap();
    let mut con = client.get_connection().unwrap();
    redis::cmd("DEL").arg(incr_key).execute(&mut con);

    for _ in 0..number_of_workers {
        let tx = tx.clone();
        let _ = thread::spawn(move || {
            let mut rng = rand::thread_rng();

            let rl = RedLock::new(vec![
                "redis://127.0.0.1:6380/",
                "redis://127.0.0.1:6381/",
                "redis://127.0.0.1:6382/",
            ]);
            let mut con = rl.servers[0].get_connection().unwrap();

            for _ in 0..number_of_incrs {
                let lock;
                loop {
                    if let Some(l) = rl.lock(resource.as_bytes(), 1000) {
                        lock = l;
                        break;
                    }
                }
                let val: i32 = redis::cmd("GET").arg(incr_key).query(&mut con).unwrap_or(0);

                let n = rng.gen_range(0..5);
                thread::sleep(Duration::from_millis(n));

                redis::cmd("SET")
                    .arg(incr_key)
                    .arg(val + 1)
                    .execute(&mut con);

                rl.unlock(&lock);
            }

            tx.send(()).unwrap();
        });
    }

    let mut i = 0;
    loop {
        if i == number_of_workers {
            break;
        }

        let _ = rx.recv();
        i += 1;
    }

    let actual_result: u32 = redis::cmd("GET").arg(incr_key).query(&mut con).unwrap_or(0);

    println!("Expected result: {}", result);
    println!("Actual result:   {}", actual_result);
    if result == actual_result {
        println!("Everything fine! \\o/");
    } else {
        println!("Something is broken. /o\\");
    }
}
